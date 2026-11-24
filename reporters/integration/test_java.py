import glob
import json
import os
import socket
import subprocess

import pytest

from conftest import REPORTERS, by_name, single_launch

JAVA = REPORTERS / "java"
SECRET = "java-reporter-secret-4711"
MVN = os.environ.get("FASTPROOF_TEST_MVN")

pytestmark = pytest.mark.skipif(not MVN, reason="FASTPROOF_TEST_MVN (path to a Maven launcher; JAVA_HOME set) is not set")


@pytest.fixture(scope="module")
def installed():
    out = subprocess.run([MVN, "-q", "-B", "-DskipTests", "install"], cwd=str(JAVA), capture_output=True, text=True, timeout=900)
    assert out.returncode == 0, out.stdout + out.stderr


def run_samples(server, key, tmp_path, profile, **extra):
    state = tmp_path / "state"
    state.mkdir(exist_ok=True)
    env = server.env(
        key,
        SAMPLE_API_TOKEN=SECRET,
        SAMPLE_STATE_DIR=str(state),
        FASTPROOF_SPOOL_DIR=str(tmp_path / "spool"),
        FASTPROOF_FLUSH_TIMEOUT_MS="8000",
        **extra,
    )
    versions = [f"-D{k}={os.environ[v]}" for k, v in (("junit.version", "FASTPROOF_TEST_JUNIT_VERSION"), ("testng.version", "FASTPROOF_TEST_TESTNG_VERSION")) if os.environ.get(v)]
    out = subprocess.run(
        [MVN, "-B", "-P", profile, *versions, "test"], cwd=str(JAVA / "samples"), env=env, capture_output=True, text=True, timeout=900,
    )
    assert out.returncode == 0, out.stdout[-3000:] + out.stderr[-2000:]
    return out


def statuses(server, key, result):
    return [a["status"] for a in server.detail(key, result["id"])["attempts"]]


def test_junit5_live_results_attempts_attachments_and_masking(server, installed, tmp_path):
    key = server.project("java-junit")
    out = run_samples(server, key, tmp_path, "junit")
    launch = single_launch(server, key)
    assert launch["status"] == "finished"
    res = by_name(server, key, launch["id"])
    names = set(res)
    assert {
        "passes with steps, logs and attachments", "failsAssertion()", "brokenByException()", "flakyPassesOnRerun()",
        "skippedByAssumption()", "nestedPasses()", "traceparentReachesBackend()",
    } <= names, names
    assert "disabled()" in names and res["disabled()"]["status"] == "skipped"
    flaky = server.detail(key, res["flakyPassesOnRerun()"]["id"])
    assert [a["status"] for a in flaky["attempts"]] == ["failed", "passed"]
    assert flaky["flakyInRun"] is True
    assert statuses(server, key, res["failsAssertion()"]) == ["failed", "failed", "failed"]
    assert res["brokenByException()"]["status"] == "broken"
    assert res["skippedByAssumption()"]["status"] == "skipped"

    p = server.detail(key, res["passes with steps, logs and attachments"]["id"])
    att = p["attempts"][0]
    note = [a for a in att["attachments"] if a["name"].startswith("note-")][0]
    assert note["name"] == "note-****.txt"
    assert server.content(key, note["id"]) == b"body with ****"
    assert "stdout line with ****" in att["stdout"]
    assert "stderr line" in att["stderr"]
    flat = json.dumps(att["steps"])
    assert "outer" in flat and "inner" in flat
    lines = server.get(
        f"/api/v1/projects/{key}/launches/{launch['id']}/logs?resultId={res['passes with steps, logs and attachments']['id']}"
    )["items"]
    streams = {(ln["stream"], ln["message"]) for ln in lines}
    assert ("stdout", "stdout line with ****") in streams, lines
    assert any(s == "log" and "system logger record x" in m for s, m in streams), lines
    assert any(s == "step" and "inner" in m for s, m in streams), lines
    assert any(a for a in p["labels"] if a["name"] == "tag" and a["value"] == "smoke"), p.keys()

    param = [r for n, r in res.items() if n.startswith("parametrized")]
    assert len(param) == 2
    values = set()
    for r in param:
        d = server.detail(key, r["id"])
        values.add(d["parameters"][0]["value"])
    assert values == {"alice", "bob"}
    assert len([n for n in res if n.startswith("repeated")]) == 2

    trace_ids = set()
    total_attempts = 0
    for r in res.values():
        d = server.detail(key, r["id"])
        assert SECRET not in json.dumps(d)
        for a in d["attempts"]:
            if a["status"] != "skipped" or r["name"] != "disabled()":
                assert a.get("traceId"), r["name"]
            if a.get("traceId"):
                trace_ids.add(a["traceId"])
            total_attempts += 1
    assert len(trace_ids) >= total_attempts - 1
    t = server.detail(key, res["traceparentReachesBackend()"]["id"])["attempts"][0]
    seen = server.content(key, [a for a in t["attachments"] if a["name"] == "seen-headers.json"][0]["id"])
    assert seen["traceparent"].split("-")[1] == t["traceId"]
    assert "launch #" in out.stderr + out.stdout or "results" in out.stdout


def test_testng_attempts_invocations_and_data_providers(server, installed, tmp_path):
    key = server.project("java-testng")
    run_samples(server, key, tmp_path, "testng")
    launch = single_launch(server, key)
    res = by_name(server, key, launch["id"])
    assert {"passesWithSteps", "failsAssertion", "flakyPassesOnRetry", "skipped"} <= set(res), set(res)
    flaky = server.detail(key, res["flakyPassesOnRetry"]["id"])
    assert [a["status"] for a in flaky["attempts"]] == ["failed", "passed"]
    assert flaky["flakyInRun"] is True
    assert res["skipped"]["status"] == "skipped"
    assert res["failsAssertion"]["status"] == "failed"
    d = server.detail(key, res["passesWithSteps"]["id"])
    assert SECRET not in json.dumps(d)
    assert "****" in json.dumps(d)
    assert "inner" in json.dumps(d["attempts"][0]["steps"])
    assert any(a for a in d["labels"] if a["name"] == "tag" and a["value"] == "smoke")
    all_results = server.results(key, launch["id"])
    assert len([r for r in all_results if r["name"] == "repeats"]) == 3
    users = [r for r in all_results if r["name"] == "parametrized"]
    assert len(users) == 2
    assert {server.detail(key, r["id"])["parameters"][0]["value"] for r in users} == {"alice", "bob"}
    for r in all_results:
        assert SECRET not in json.dumps(server.detail(key, r["id"]))


def test_testplan_selects_tests(server, installed, tmp_path):
    key = server.project("java-plan")
    plan = tmp_path / "testplan.json"
    plan.write_text(json.dumps({"version": "1.0", "tests": [{"selector": "sample.JUnitSampleTest#failsAssertion"}]}))
    out = run_samples(server, key, tmp_path, "junit", FASTPROOF_TESTPLAN=str(plan))
    launch = single_launch(server, key)
    assert set(by_name(server, key, launch["id"])) == {"failsAssertion()"}, "\n".join(ln for ln in (out.stdout + out.stderr).splitlines() if "[fastproof]" in ln)


def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def test_server_down_spools_and_the_flush_command_sends(server, installed, tmp_path):
    key = server.project("java-spool")
    state = tmp_path / "state"
    state.mkdir()
    spool = tmp_path / "spool"
    env = server.env(
        key, SAMPLE_API_TOKEN=SECRET, SAMPLE_STATE_DIR=str(state), FASTPROOF_SPOOL_DIR=str(spool),
        FASTPROOF_FLUSH_TIMEOUT_MS="3000", FASTPROOF_URL=f"http://127.0.0.1:{free_port()}", FASTPROOF_LAUNCH="java-spool-1",
    )
    out = subprocess.run([MVN, "-B", "-P", "testng", "test"], cwd=str(JAVA / "samples"), env=env, capture_output=True, text=True, timeout=900)
    assert out.returncode == 0, out.stdout[-3000:]
    assert server.launches(key) == []
    assert glob.glob(str(spool / "*" / "results.ndjson"))
    assert SECRET not in "".join(open(p).read() for p in glob.glob(str(spool / "*" / "*.json*")))
    jar = glob.glob(str(JAVA / "core" / "target" / "fastproof-core-*.jar"))
    jar = [j for j in jar if "tests" not in j and "sources" not in j][0]
    java = os.path.join(os.environ.get("JAVA_HOME", ""), "bin", "java") if os.environ.get("JAVA_HOME") else "java"
    flush_env = server.env(key, FASTPROOF_SPOOL_DIR=str(spool))
    flush = subprocess.run([java, "-jar", jar, "flush"], env=flush_env, capture_output=True, text=True, timeout=120)
    assert flush.returncode == 0, flush.stdout + flush.stderr
    assert "sent" in flush.stdout and "0 left" in flush.stdout, flush.stdout + flush.stderr
    launch = single_launch(server, key)
    res = by_name(server, key, launch["id"])
    assert {"passesWithSteps", "failsAssertion", "flakyPassesOnRetry"} <= set(res)
    assert not glob.glob(str(spool / "*" / "results.ndjson"))
    d = server.detail(key, res["passesWithSteps"]["id"])
    assert SECRET not in json.dumps(d)
