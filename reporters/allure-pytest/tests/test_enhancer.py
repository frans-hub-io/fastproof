import json
import os
import socket

import pytest

from fake_server import FakeServer

pytest_plugins = ["pytester"]

TOKEN = "test-token-abcdef"
SECRET = "correct-horse-battery"

SAMPLE = """
import logging
import os

import allure
import pytest

log = logging.getLogger("sample")


def test_pass():
    log.info("hello %s", os.environ["SHOP_PASSWORD"])
    with allure.step("outer"):
        with allure.step("inner"):
            print("out " + os.environ["SHOP_PASSWORD"])
    allure.attach("pw=" + os.environ["SHOP_PASSWORD"], name="note.txt", attachment_type=allure.attachment_type.TEXT)
    allure.attach(b"x" * 100, name="same.bin", attachment_type=allure.attachment_type.PNG)
    allure.dynamic.parameter("token", os.environ["SHOP_PASSWORD"], mode=allure.parameter_mode.MASKED)


def test_fail():
    allure.attach(b"x" * 100, name="same.bin", attachment_type=allure.attachment_type.PNG)
    assert 1 == 2


@pytest.mark.flaky(reruns=2)
def test_flaky():
    path = os.path.join(os.path.dirname(__file__), "flaky.count")
    n = int(open(path).read()) if os.path.exists(path) else 0
    open(path, "w").write(str(n + 1))
    log.warning("attempt %s", n)
    assert n >= 1


@allure.label("as_id", "ID-7")
def test_with_id():
    pass


@pytest.mark.parametrize("browser", ["chrome", "firefox"])
def test_param(browser):
    pass


def test_skipped():
    pytest.skip("not today")
"""


@pytest.fixture
def env(pytester, monkeypatch, tmp_path):
    for k in list(os.environ):
        if k.startswith(("FASTPROOF_", "ALLURE_")) or k in ("GITLAB_CI", "GITHUB_ACTIONS", "JENKINS_URL"):
            monkeypatch.delenv(k, raising=False)
    monkeypatch.setenv("FASTPROOF_TOKEN", TOKEN)
    monkeypatch.setenv("FASTPROOF_PROJECT", "demo")
    monkeypatch.setenv("FASTPROOF_SPOOL_DIR", str(tmp_path / "spool"))
    monkeypatch.setenv("FASTPROOF_FLUSH_TIMEOUT_MS", "8000")
    monkeypatch.setenv("SHOP_PASSWORD", SECRET)
    pytester.makepyfile(test_sample=SAMPLE)
    pytester.makeini("[pytest]\n")
    return monkeypatch


def by_name(fake):
    out = {}
    for r in fake.finals():
        out.setdefault(r["name"], []).append(r)
    for v in out.values():
        v.sort(key=lambda r: r["retry"])
    return out


def run(pytester, *args):
    return pytester.runpytest_subprocess("-p", "no:cacheprovider", "--alluredir=allure-results", *args)


def test_live_attempts_steps_logs_masking_and_allure_results(pytester, env):
    with FakeServer(token=TOKEN, logs=True, running=True) as fake:
        env.setenv("FASTPROOF_URL", fake.url)
        res = run(pytester)
        o = res.parseoutcomes()
        assert (o.get("passed"), o.get("failed"), o.get("skipped"), o.get("rerun")) == (5, 1, 1, 1), o
        assert len(fake.launches) == 1
        assert next(iter(fake.launches.values()))["finished"] == "finished"
        r = by_name(fake)
        assert [a["status"] for a in r["test_flaky"]] == ["failed", "passed"]
        assert [a["retry"] for a in r["test_flaky"]] == [0, 1]
        assert len({a["id"] for a in r["test_flaky"]}) == 2
        for attempts in r.values():
            for a in attempts:
                assert fake.results[a["id"]]["history"][0] == "running"
                assert a["traceparent"].startswith("00-")
        assert len({a["traceparent"].split("-")[1] for att in r.values() for a in att}) == 8
        p = r["test_pass"][0]
        assert {a["name"] for a in p["attachments"]} == {"note.txt", "same.bin", "stdout", "log"}
        out = [a for a in fake.attachments.values() if a["name"] == "stdout"][0]
        assert out["body"] == b"out ****\n"
        assert p["steps"][0]["name"] == "outer" and p["steps"][0]["steps"][0]["name"] == "inner"
        assert {"name": "token", "value": "****", "mode": "masked"} in p["parameters"]
        dump = json.dumps(fake.finals()) + json.dumps(fake.logs) + json.dumps(list(fake.running_lines.values()))
        assert SECRET not in dump
        note = [a for a in fake.attachments.values() if a["name"] == "note.txt"][0]
        assert note["body"] == b"pw=****"
        assert r["test_fail"][0]["status"] == "failed" and "assert 1 == 2" in r["test_fail"][0]["error"]["message"]
        assert r["test_skipped"][0]["status"] == "skipped"
        assert any(lb == {"name": "as_id", "value": "ID-7"} for lb in r["test_with_id"][0]["labels"])
        for name, attempts in r.items():
            for a in attempts:
                first = fake.running_lines[a["id"]]
                assert first["fullName"] == a["fullName"], name
                assert first["testCaseId"] == a["testCaseId"], name
                assert first.get("parameters") == [x for x in a["parameters"] if x["name"] != "token"], name
        logs = [ln for ln in fake.logs if ln["resultId"] == p["id"]]
        assert [ln["seq"] for ln in logs] == list(range(len(logs)))
        assert any(ln["stream"] == "step" and ln.get("step") == "outer › inner" for ln in logs)
        assert any(ln["stream"] == "log" and "hello ****" in ln["message"] for ln in logs)
        flaky_logs = [ln for ln in fake.logs if ln["resultId"] in {a["id"] for a in r["test_flaky"]}]
        assert {ln["attempt"] for ln in flaky_logs} == {0, 1}
        written = [json.loads(f.read_text()) for f in (pytester.path / "allure-results").glob("*-result.json")]
        assert len(written) == 8
        for w in written:
            assert any(lb["name"] == "traceparent" and lb["value"].startswith("00-") for lb in w["labels"])
        ids = {lb["value"] for w in written for lb in w["labels"] if lb["name"] == "traceparent"}
        assert ids == {a["traceparent"] for att in r.values() for a in att}


def test_trace_label_can_be_turned_off(pytester, env):
    with FakeServer(token=TOKEN) as fake:
        env.setenv("FASTPROOF_URL", fake.url)
        env.setenv("FASTPROOF_ALLURE_TRACE_LABEL", "false")
        run(pytester, "-k", "test_pass")
        written = [json.loads(f.read_text()) for f in (pytester.path / "allure-results").glob("*-result.json")]
        assert written and not any(lb["name"] == "traceparent" for w in written for lb in w["labels"])
        assert by_name(fake)["test_pass"][0]["traceparent"].startswith("00-")


def test_stage_b_server_has_no_running_or_logs(pytester, env):
    with FakeServer(logs=False, running=False) as fake:
        env.setenv("FASTPROOF_URL", fake.url)
        env.setenv("FASTPROOF_LAUNCH", "ci-run-1")
        run(pytester)
        assert all(not x.endswith(":running") for x in fake.order)
        assert not fake.logs
        assert len(fake.finals()) == 8
        launch = next(iter(fake.launches.values()))
        assert launch["externalId"] == "ci-run-1" and "finished" not in launch


def test_without_alluredir_pytest_fastproof_reports_as_before(pytester, env):
    with FakeServer(token=TOKEN) as fake:
        env.setenv("FASTPROOF_URL", fake.url)
        res = pytester.runpytest_subprocess("-p", "no:cacheprovider", "-k", "test_pass")
        assert res.parseoutcomes()["passed"] == 1
        assert [r["id"][:3] for r in fake.finals()] == ["py-"]


def test_server_down_spools(pytester, env, tmp_path):
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    env.setenv("FASTPROOF_URL", f"http://127.0.0.1:{port}")
    env.setenv("FASTPROOF_FLUSH_TIMEOUT_MS", "3000")
    res = run(pytester)
    assert res.ret == 1
    res.stdout.fnmatch_lines(["*saved to*"])
    sessions = [p for p in (tmp_path / "spool").iterdir() if not p.name.startswith(".")]
    assert len(sessions) == 1
    assert len((sessions[0] / "results.ndjson").read_text().splitlines()) == 8


def test_xdist_workers_share_one_launch(pytester, env):
    with FakeServer(token=TOKEN, logs=True, running=True) as fake:
        env.setenv("FASTPROOF_URL", fake.url)
        res = run(pytester, "-n", "2")
        assert res.parseoutcomes()["passed"] == 5
        assert len(fake.launches) == 1
        launch = next(iter(fake.launches.values()))
        assert launch["finished"] == "finished"
        assert len(fake.finals()) == 8
        assert not any(x.endswith(":running") and fake.results[x.rsplit(":", 1)[0]]["status"] == "running" for x in fake.order)
