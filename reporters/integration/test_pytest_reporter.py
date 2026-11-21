import json
import subprocess
import sys

from conftest import REPORTERS, by_name, free_port, single_launch, wait_for

SAMPLES = REPORTERS / "pytest" / "samples"
SECRET = "py-integration-secret-77"


def run_pytest(server, key, tmp_path, *args, url=None):
    state = tmp_path / "state"
    state.mkdir(exist_ok=True)
    env = server.env(
        key,
        SAMPLE_API_TOKEN=SECRET,
        SAMPLE_STATE_DIR=str(state),
        FASTPROOF_SPOOL_DIR=str(tmp_path / "spool"),
        FASTPROOF_FLUSH_TIMEOUT_MS="5000",
    )
    if url:
        env["FASTPROOF_URL"] = url
    return subprocess.run(
        [sys.executable, "-m", "pytest", "-p", "no:cacheprovider", "-q", str(SAMPLES / "test_sample.py"), *args],
        cwd=str(SAMPLES), env=env, capture_output=True, text=True, timeout=300,
    )


def check_launch(server, key, threads):
    launch = single_launch(server, key)
    assert launch["status"] == "finished"
    c = launch["counts"]
    assert (c["total"], c["passed"], c["failed"], c["broken"], c["skipped"], c["flaky"]) == (9, 5, 1, 1, 2, 1)
    res = by_name(server, key, launch["id"])
    assert sum(r["attemptCount"] for r in res.values()) == 10
    flaky = server.detail(key, res["test_flaky_passes_on_rerun"]["id"])
    assert [a["status"] for a in flaky["attempts"]] == ["failed", "passed"]
    assert flaky["flakyInRun"] is True
    for r in res.values():
        d = server.detail(key, r["id"])
        assert SECRET not in json.dumps(d)
        assert {lb["value"] for lb in d["labels"] if lb["name"] == "thread"} <= threads
        for a in d["attempts"]:
            assert a.get("traceId"), r["name"]
    p = server.detail(key, res["test_passes_with_logs_steps_and_attachments"]["id"])
    att = p["attempts"][0]
    assert {a["name"] for a in att["attachments"]} == {"note-****.txt", "shared.bin"}
    note = [a for a in att["attachments"] if a["name"] == "note-****.txt"][0]
    assert server.content(key, note["id"]) == b"body with ****"
    assert "stdout line with ****" in att["stdout"]
    assert "--- log ---" in att["stdout"] and "starting with token ****" in att["stdout"]
    assert att["steps"][0]["name"] == "outer" and att["steps"][0]["steps"][0]["name"] == "inner"
    shared = [a["id"] for a in att["attachments"] if a["name"] == "shared.bin"]
    failed = server.detail(key, res["test_fails_assertion"]["id"])
    assert failed["attempts"][0]["attachments"][0]["size"] == 2048
    assert shared
    tp = server.detail(key, res["test_traceparent_reaches_backend"]["id"])["attempts"][0]
    seen = server.content(key, tp["attachments"][0]["id"])
    assert seen["traceparent"].split("-")[1] == tp["traceId"]
    assert "fastproof.attempt=0" in seen["baggage"]
    params = server.detail(key, res["test_parametrized[alice-****]"]["id"])["parameters"]
    assert {"name": "password", "value": "****"} in [{"name": x["name"], "value": x.get("value")} for x in params]
    broken = server.detail(key, res["test_broken_by_exception"]["id"])
    assert broken["status"] == "broken" and "backend exploded" in broken["attempts"][0]["error"]["message"]


def test_single_process_with_reruns(server, tmp_path):
    key = server.project("py-single")
    out = run_pytest(server, key, tmp_path)
    assert out.returncode == 1, out.stdout + out.stderr
    assert "[fastproof] launch #1: 10 results" in out.stdout, out.stdout
    check_launch(server, key, {"main"})


def test_xdist_two_workers_one_launch(server, tmp_path):
    key = server.project("py-xdist")
    out = run_pytest(server, key, tmp_path, "-n", "2")
    assert out.returncode == 1, out.stdout + out.stderr
    check_launch(server, key, {"gw0", "gw1"})


def test_server_down_run_passes_and_spool_is_flushed(server, tmp_path):
    key = server.project("py-offline")
    dead = f"http://127.0.0.1:{free_port()}"
    out = run_pytest(server, key, tmp_path, "-k", "passes_with or parametrized or skipped", url=dead)
    assert out.returncode == 0, out.stdout + out.stderr
    assert "saved to" in out.stdout and "python -m pytest_fastproof flush" in out.stdout
    assert server.launches(key) == []
    env = server.env(key, FASTPROOF_SPOOL_DIR=str(tmp_path / "spool"))
    flush = subprocess.run(
        [sys.executable, "-m", "pytest_fastproof", "flush"], env=env, capture_output=True, text=True, timeout=120,
    )
    assert flush.returncode == 0, flush.stdout + flush.stderr
    assert "sent 4 results" in flush.stdout
    launch = wait_for(lambda: server.launches(key))[0]
    assert launch["status"] == "finished" and launch["counts"]["total"] == 4
    res = by_name(server, key, launch["id"])
    p = server.detail(key, res["test_passes_with_logs_steps_and_attachments"]["id"])
    assert {a["name"] for a in p["attempts"][0]["attachments"]} == {"note-****.txt", "shared.bin"}
    assert not [d for d in (tmp_path / "spool").iterdir() if not d.name.startswith(".")]


def test_testplan_runs_only_planned_tests(server, tmp_path):
    key = server.project("py-plan")
    plan = tmp_path / "testplan.json"
    plan.write_text(json.dumps({"version": "1.0", "tests": [{"selector": "test_sample.py::test_skipped"},
                                                              {"selector": "test_sample#test_parametrized"}]}))
    out = run_pytest(server, key, tmp_path, f"--fastproof-testplan={plan}")
    assert out.returncode == 0, out.stdout + out.stderr
    launch = single_launch(server, key)
    assert sorted(r["name"] for r in server.results(key, launch["id"])) == [
        "test_parametrized[alice-****]", "test_parametrized[bob-plain-password-value]", "test_skipped",
    ]
