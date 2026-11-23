import json
import os
import socket
import subprocess
import sys

import pytest

from fake_server import FakeServer

pytest_plugins = ["pytester"]

TOKEN = "test-token-abcdef"

SAMPLE = """
import logging
import os

import pytest

import pytest_fastproof as fastproof

log = logging.getLogger("sample")


def test_pass(fastproof_trace):
    log.info("hello %s", os.environ["SHOP_PASSWORD"])
    with fastproof.step("outer"):
        with fastproof.step("inner"):
            print("out " + os.environ["SHOP_PASSWORD"])
    fastproof.attach(b"x" * 100, name="same.bin")
    fastproof.attach("pw=" + os.environ["SHOP_PASSWORD"], name="note.txt")
    t = fastproof.trace()
    assert t is None or t is fastproof_trace


def test_fail():
    fastproof.attach(b"x" * 100, name="same.bin")
    assert 1 == 2


@pytest.mark.flaky(reruns=2)
def test_flaky():
    path = os.path.join(os.path.dirname(__file__), "flaky.count")
    n = int(open(path).read()) if os.path.exists(path) else 0
    open(path, "w").write(str(n + 1))
    log.warning("attempt %s", n)
    assert n >= 1


@pytest.mark.fastproof_id("ID-7")
def test_with_id():
    pass


@pytest.mark.parametrize("browser", ["chrome", "firefox"])
def test_param(browser):
    pass
"""


@pytest.fixture
def env(pytester, monkeypatch, tmp_path):
    for k in list(os.environ):
        if k.startswith("FASTPROOF_") or k in ("GITLAB_CI", "GITHUB_ACTIONS", "JENKINS_URL"):
            monkeypatch.delenv(k, raising=False)
    monkeypatch.setenv("FASTPROOF_TOKEN", TOKEN)
    monkeypatch.setenv("FASTPROOF_PROJECT", "demo")
    monkeypatch.setenv("FASTPROOF_SPOOL_DIR", str(tmp_path / "spool"))
    monkeypatch.setenv("FASTPROOF_FLUSH_TIMEOUT_MS", "8000")
    monkeypatch.setenv("SHOP_PASSWORD", "correct-horse-battery")
    pytester.makepyfile(test_sample=SAMPLE)
    pytester.makeini("[pytest]\n")
    return monkeypatch


def assert_run(res):
    o = res.parseoutcomes()
    assert (o.get("passed"), o.get("failed"), o.get("rerun")) == (5, 1, 1), o


def by_name(fake):
    out = {}
    for r in fake.finals():
        out.setdefault(r["name"], []).append(r)
    for v in out.values():
        v.sort(key=lambda r: r["retry"])
    return out


def test_live_path_running_logs_attempts_attachments(pytester, env):
    with FakeServer(token=TOKEN, logs=True, running=True) as fake:
        env.setenv("FASTPROOF_URL", fake.url)
        res = pytester.runpytest_inprocess("-p", "no:cacheprovider")
        assert_run(res)
        assert len(fake.launches) == 1
        launch = next(iter(fake.launches.values()))
        assert launch["finished"] == "finished"
        r = by_name(fake)
        assert [a["status"] for a in r["test_flaky"]] == ["failed", "passed"]
        assert [a["retry"] for a in r["test_flaky"]] == [0, 1]
        assert len({a["id"] for a in r["test_flaky"]}) == 2
        for attempts in r.values():
            for a in attempts:
                assert fake.results[a["id"]]["history"][0] == "running"
                assert a["traceparent"].startswith("00-")
        p = r["test_pass"][0]
        assert {a["name"] for a in p["attachments"]} == {"same.bin", "note.txt"}
        assert "correct-horse-battery" not in json.dumps(fake.finals())
        assert "correct-horse-battery" not in json.dumps(fake.logs)
        note = [a for a in fake.attachments.values() if a["name"] == "note.txt"][0]
        assert note["body"] == b"pw=****"
        assert fake.uploads == 2
        assert p["stdout"] == "out ****\n"
        assert p["steps"][0]["name"] == "outer" and p["steps"][0]["steps"][0]["name"] == "inner"
        assert r["test_fail"][0]["status"] == "failed"
        assert r["test_with_id"][0]["testCaseId"] == "ID-7"
        # The running line already carries the final identity (explicit id, parameters).
        for name, attempts in r.items():
            for a in attempts:
                first = fake.running_lines[a["id"]]
                assert first.get("testCaseId") == a.get("testCaseId"), name
                assert first.get("parameters") == a.get("parameters"), name
                assert first["fullName"] == a["fullName"], name
        assert fake.running_lines[r["test_param[firefox]"][0]["id"]]["parameters"] == [
            {"name": "browser", "value": "firefox"}
        ]
        logs = [ln for ln in fake.logs if ln["resultId"] == p["id"]]
        assert [ln["seq"] for ln in logs] == list(range(len(logs)))
        assert any(ln["stream"] == "step" and ln.get("step") == "outer › inner" for ln in logs)
        assert any(ln["stream"] == "log" and "hello ****" in ln["message"] for ln in logs)
        flaky_logs = [ln for ln in fake.logs if ln["resultId"] in {a["id"] for a in r["test_flaky"]}]
        assert {ln["attempt"] for ln in flaky_logs} == {0, 1}


def test_stage_b_server_falls_back_to_stdout(pytester, env):
    with FakeServer(logs=False, running=False) as fake:
        env.setenv("FASTPROOF_URL", fake.url)
        env.setenv("FASTPROOF_LAUNCH", "ci-run-1")
        res = pytester.runpytest_inprocess("-p", "no:cacheprovider")
        assert_run(res)
        r = by_name(fake)
        assert all(not x.endswith(":running") for x in fake.order)
        assert not fake.logs
        flaky = r["test_flaky"]
        assert "--- log ---" in flaky[1]["stdout"] and "attempt 1" in flaky[1]["stdout"]
        assert "attempt 0" not in flaky[1]["stdout"]
        launch = next(iter(fake.launches.values()))
        assert launch["externalId"] == "ci-run-1"
        assert "finished" not in launch


def _free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def test_server_down_spools_and_flushes_later(pytester, env, tmp_path):
    port = _free_port()
    env.setenv("FASTPROOF_URL", f"http://127.0.0.1:{port}")
    env.setenv("FASTPROOF_FLUSH_TIMEOUT_MS", "3000")
    res = pytester.runpytest_inprocess("-p", "no:cacheprovider")
    assert_run(res)
    assert res.ret == 1
    res.stdout.fnmatch_lines(["*saved to*", "*python -m pytest_fastproof flush*"])
    spool = tmp_path / "spool"
    sessions = [p for p in spool.iterdir() if not p.name.startswith(".")]
    assert len(sessions) == 1
    assert len((sessions[0] / "results.ndjson").read_text().splitlines()) == 7
    with FakeServer(token=TOKEN, port=port) as fake:
        # The spool knows its project: FASTPROOF_PROJECT is not needed to flush it.
        flush_env = {k: v for k, v in os.environ.items() if k != "FASTPROOF_PROJECT"}
        out = subprocess.run(
            [sys.executable, "-m", "pytest_fastproof", "flush", "--spool", str(spool)],
            capture_output=True, text=True, env=flush_env, timeout=60,
        )
        assert out.returncode == 0, out.stderr
        assert "sent 7 results" in out.stdout
        assert len(fake.finals()) == 7
        assert {a["name"] for a in by_name(fake)["test_pass"][0]["attachments"]} == {"same.bin", "note.txt"}
        assert next(iter(fake.launches.values()))["finished"] == "finished"
    assert not [p for p in spool.iterdir() if not p.name.startswith(".")]


def test_testplan_deselects(pytester, env):
    plan = pytester.path / "testplan.json"
    plan.write_text(json.dumps({"version": "1.0", "tests": [{"selector": "test_sample.py::test_pass"}, {"id": "ID-7"}]}))
    env.setenv("FASTPROOF_ENABLED", "false")
    res = pytester.runpytest_inprocess("-p", "no:cacheprovider", "--fastproof-testplan=testplan.json")
    res.assert_outcomes(passed=2, deselected=4)


def test_disabled_without_url(pytester, env):
    res = pytester.runpytest_inprocess("-p", "no:cacheprovider")
    assert_run(res)


def test_xdist_one_launch_no_duplicates(pytester, env):
    with FakeServer(token=TOKEN, logs=True, running=True) as fake:
        env.setenv("FASTPROOF_URL", fake.url)
        res = pytester.runpytest_subprocess("-p", "no:cacheprovider", "-n", "2")
        assert_run(res)
        assert len(fake.launches) == 1
        finals = fake.finals()
        assert len(finals) == 7
        assert len({r["id"] for r in finals}) == 7
        r = by_name(fake)
        assert [a["status"] for a in r["test_flaky"]] == ["failed", "passed"]
        assert {lb["value"] for x in finals for lb in x["labels"] if lb["name"] == "thread"} <= {"gw0", "gw1"}
        assert any(ln["resultId"] == r["test_pass"][0]["id"] for ln in fake.logs)
        assert "correct-horse-battery" not in json.dumps(finals) + json.dumps(fake.logs)


def test_hint_when_results_also_go_to_alluredir(pytester, env):
    pytester.makeconftest(
        "def pytest_addoption(parser):\n"
        "    parser.addoption('--alluredir', dest='allure_report_dir', default=None)\n"
    )
    with FakeServer(token=TOKEN, logs=True, running=True) as fake:
        env.setenv("FASTPROOF_URL", fake.url)
        res = pytester.runpytest_inprocess("-p", "no:cacheprovider", "-p", "no:fastproof_allure_pytest", "-p", "no:allure_pytest", "--alluredir", "allure-results", "-k", "test_pass")
        res.stdout.fnmatch_lines(["*also written to --alluredir*FASTPROOF_ENABLED=false*"])
