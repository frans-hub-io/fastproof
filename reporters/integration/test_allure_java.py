import json
import os
import subprocess

import pytest

from conftest import REPORTERS, by_name, single_launch

AJ = REPORTERS / "allure-java"
SECRET = "allure-java-secret-9090"
MVN = os.environ.get("FASTPROOF_TEST_MVN")

pytestmark = pytest.mark.skipif(not MVN, reason="FASTPROOF_TEST_MVN (path to a Maven launcher; JAVA_HOME set) is not set")


@pytest.fixture(scope="module")
def installed():
    out = subprocess.run([MVN, "-q", "-B", "-DskipTests", "install"], cwd=str(AJ), capture_output=True, text=True, timeout=900)
    assert out.returncode == 0, out.stdout + out.stderr


def run_samples(server, key, tmp_path, profile):
    state = tmp_path / "state"
    state.mkdir()
    env = server.env(
        key,
        SAMPLE_API_TOKEN=SECRET,
        SAMPLE_STATE_DIR=str(state),
        FASTPROOF_SPOOL_DIR=str(tmp_path / "spool"),
        FASTPROOF_FLUSH_TIMEOUT_MS="8000",
    )
    out = subprocess.run(
        [MVN, "-B", "-P", profile, f"-Dallure.results.directory={tmp_path / 'allure-results'}", "test"],
        cwd=str(AJ / "samples"), env=env, capture_output=True, text=True, timeout=900,
    )
    assert out.returncode == 0, out.stdout[-3000:] + out.stderr[-2000:]
    return out


def test_junit5_with_surefire_reruns(server, installed, tmp_path):
    key = server.project("aj-junit")
    out = run_samples(server, key, tmp_path, "junit")
    launch = single_launch(server, key)
    assert launch["status"] == "finished"
    res = by_name(server, key, launch["id"])
    names = set(res)
    assert {"passesWithStepsAndAttachments()", "failsAssertion()", "flakyPassesOnRerun()"} <= names, names
    flaky = server.detail(key, res["flakyPassesOnRerun()"]["id"])
    assert [a["status"] for a in flaky["attempts"]] == ["failed", "passed"]
    assert flaky["flakyInRun"] is True
    fails = server.detail(key, res["failsAssertion()"]["id"])
    assert [a["status"] for a in fails["attempts"]] == ["failed", "failed", "failed"]
    assert res["skipped()"]["status"] == "skipped"
    p = server.detail(key, res["passesWithStepsAndAttachments()"]["id"])
    att = p["attempts"][0]
    note = [a for a in att["attachments"] if a["name"].startswith("note-")][0]
    assert note["name"] == "note-****.txt"
    assert server.content(key, note["id"]) == b"body with ****"
    flat = json.dumps(att["steps"])
    assert "outer" in flat and "inner" in flat
    lines = server.get(
        f"/api/v1/projects/{key}/launches/{launch['id']}/logs?resultId={res['passesWithStepsAndAttachments()']['id']}"
    )["items"]
    assert any(ln["stream"] == "step" and "inner" in ln["message"] for ln in lines), lines
    trace_ids = set()
    for r in res.values():
        d = server.detail(key, r["id"])
        assert SECRET not in json.dumps(d)
        for a in d["attempts"]:
            assert a.get("traceId"), r["name"]
            trace_ids.add(a["traceId"])
    assert len(trace_ids) == sum(r["attemptCount"] for r in res.values())
    t = server.detail(key, res["traceparentReachesBackend()"]["id"])["attempts"][0]
    seen = server.content(key, [a for a in t["attachments"] if a["name"] == "seen-headers.json"][0]["id"])
    assert seen["traceparent"].split("-")[1] == t["traceId"]
    written = list((tmp_path / "allure-results").glob("*-result.json"))
    assert len(written) == sum(r["attemptCount"] for r in res.values())
    assert all(any(lb["name"] == "traceparent" for lb in json.loads(f.read_text())["labels"]) for f in written)


def test_testng_with_surefire_reruns(server, installed, tmp_path):
    key = server.project("aj-testng")
    run_samples(server, key, tmp_path, "testng")
    launch = single_launch(server, key)
    res = by_name(server, key, launch["id"])
    assert {"passesWithSteps", "failsAssertion", "flakyPassesOnRerun"} <= set(res), set(res)
    flaky = server.detail(key, res["flakyPassesOnRerun"]["id"])
    assert len(flaky["attempts"]) == 2 and flaky["attempts"][1]["status"] == "passed"
    assert flaky["attempts"][0]["status"] == "skipped"
    assert "****" in json.dumps(server.detail(key, res["passesWithSteps"]["id"]))
    for r in res.values():
        assert SECRET not in json.dumps(server.detail(key, r["id"]))
