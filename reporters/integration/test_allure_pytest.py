import json
import subprocess
import sys

from conftest import REPORTERS, by_name, single_launch

SAMPLES = REPORTERS / "allure-pytest" / "samples"
SECRET = "allure-py-secret-4321"


def run_pytest(server, key, tmp_path, *args):
    state = tmp_path / "state"
    state.mkdir(exist_ok=True)
    env = server.env(
        key,
        SAMPLE_API_TOKEN=SECRET,
        SAMPLE_STATE_DIR=str(state),
        FASTPROOF_SPOOL_DIR=str(tmp_path / "spool"),
        FASTPROOF_FLUSH_TIMEOUT_MS="5000",
    )
    results = tmp_path / "allure-results"
    return subprocess.run(
        [sys.executable, "-m", "pytest", "-p", "no:cacheprovider", "-q", f"--alluredir={results}", str(SAMPLES / "test_sample.py"), *args],
        cwd=str(SAMPLES), env=env, capture_output=True, text=True, timeout=300,
    ), results


def check(server, key, results):
    launch = single_launch(server, key)
    assert launch["status"] == "finished"
    c = launch["counts"]
    assert (c["total"], c["passed"], c["failed"], c["broken"], c["skipped"], c["flaky"]) == (9, 6, 1, 1, 1, 1)
    res = by_name(server, key, launch["id"])
    assert sum(r["attemptCount"] for r in res.values()) == 10
    flaky = server.detail(key, res["test_flaky_passes_on_rerun"]["id"])
    assert [a["status"] for a in flaky["attempts"]] == ["failed", "passed"]
    assert flaky["flakyInRun"] is True
    trace_ids = set()
    for r in res.values():
        d = server.detail(key, r["id"])
        assert SECRET not in json.dumps(d)
        for a in d["attempts"]:
            assert a.get("traceId"), r["name"]
            trace_ids.add(a["traceId"])
    assert len(trace_ids) == 10
    p = server.detail(key, res["test_passes_with_steps_and_attachments"]["id"])
    att = p["attempts"][0]
    names = {a["name"] for a in att["attachments"]}
    assert {"note-****.txt", "shared.bin"} <= names
    note = [a for a in att["attachments"] if a["name"] == "note-****.txt"][0]
    assert server.content(key, note["id"]) == b"body with ****"
    lines = server.get(
        f"/api/v1/projects/{key}/launches/{launch['id']}/logs?resultId={res['test_passes_with_steps_and_attachments']['id']}"
    )["items"]
    assert any(ln["stream"] == "step" and ln["message"].startswith("▶ outer") for ln in lines), lines
    assert any("starting with token ****" in ln["message"] for ln in lines), lines
    assert att["steps"][0]["name"] == "outer" and att["steps"][0]["steps"][0]["name"] == "inner"
    tp = server.detail(key, res["test_traceparent_reaches_backend"]["id"])["attempts"][0]
    seen_att = [a for a in tp["attachments"] if a["name"] == "seen-headers.json"][0]
    seen = server.content(key, seen_att["id"])
    assert seen["traceparent"].split("-")[1] == tp["traceId"]
    assert "fastproof.attempt=0" in seen["baggage"]
    written = [json.loads(f.read_text()) for f in results.glob("*-result.json")]
    assert len(written) == 10
    assert {lb["value"].split("-")[1] for w in written for lb in w["labels"] if lb["name"] == "traceparent"} == trace_ids


def test_single_process_with_reruns(server, tmp_path):
    key = server.project("apy-single")
    out, results = run_pytest(server, key, tmp_path)
    assert out.returncode == 1, out.stdout + out.stderr
    assert "[fastproof] launch #1: 10 results" in out.stdout, out.stdout
    check(server, key, results)


def test_xdist_two_workers_one_launch(server, tmp_path):
    key = server.project("apy-xdist")
    out, results = run_pytest(server, key, tmp_path, "-n", "2")
    assert out.returncode == 1, out.stdout + out.stderr
    check(server, key, results)
