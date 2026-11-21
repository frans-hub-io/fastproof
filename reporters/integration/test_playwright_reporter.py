import json
import os
import shutil
import subprocess

import pytest

from conftest import REPORTERS, by_name, free_port, single_launch, wait_for

PW = REPORTERS / "playwright"
SECRET = "pw-integration-secret-55"
BROWSER = os.environ.get("FASTPROOF_TEST_BROWSER") == "1"

pytestmark = pytest.mark.skipif(
    not (PW / "node_modules" / "@playwright" / "test").is_dir() or not (PW / "dist" / "index.js").is_file(),
    reason="run `npm ci && npm run build` in reporters/playwright first",
)


def run_pw(server, key, tmp_path, *args, url=None, extra=None):
    env = server.env(
        key,
        E2E_SECRET_TOKEN=SECRET,
        E2E_OUTPUT_DIR=str(tmp_path / "out"),
        FASTPROOF_SPOOL_DIR=str(tmp_path / "spool"),
        FASTPROOF_FLUSH_TIMEOUT_MS="8000",
    )
    if url:
        env["FASTPROOF_URL"] = url
    if BROWSER:
        env["E2E_BROWSER"] = "1"
    env.update(extra or {})
    npx = shutil.which("npx") or "npx"
    return subprocess.run(
        [npx, "playwright", "test", "-c", "e2e", *args],
        cwd=str(PW), env=env, capture_output=True, text=True, timeout=600,
    )


def test_live_run_attempts_attachments_trace_and_masking(server, tmp_path):
    key = server.project("pw-live")
    out = run_pw(server, key, tmp_path)
    assert out.returncode == 1, out.stdout + out.stderr
    total = 6 if BROWSER else 5
    assert f"[fastproof] launch #1: {total + 2} results" in out.stderr + out.stdout, out.stdout + out.stderr
    launch = single_launch(server, key)
    assert launch["status"] == "finished"
    c = launch["counts"]
    assert (c["total"], c["failed"], c["skipped"], c["flaky"]) == (total, 1, 1, 1)
    res = by_name(server, key, launch["id"])
    assert sum(r["attemptCount"] for r in res.values()) == total + 2
    fails = server.detail(key, res["fails every time @smoke"]["id"])
    assert [a["status"] for a in fails["attempts"]] == ["failed", "failed"]
    assert {lb["value"] for lb in fails["labels"] if lb["name"] == "tag"} == {"smoke"}
    flaky = server.detail(key, res["flaky: passes on retry"]["id"])
    assert [a["status"] for a in flaky["attempts"]] == ["failed", "passed"]
    assert "first attempt fails" in flaky["attempts"][0]["error"]["message"]
    assert flaky["attempts"][0]["traceId"] != flaky["attempts"][1]["traceId"]
    passes = server.detail(key, res["passes with steps and output"]["id"])
    att = passes["attempts"][0]
    assert "using token **** and ****" in att["stdout"]
    assert {a["name"] for a in att["attachments"]} >= {"note-****", "shared.bin"}
    note = [a for a in att["attachments"] if a["name"] == "note-****"][0]
    assert server.content(key, note["id"]) == b"body with ****"
    step_names = [s["name"] for s in att["steps"]]
    assert "outer step" in step_names
    outer = att["steps"][step_names.index("outer step")]
    assert outer["steps"][0]["name"] == "inner step"
    shared_ids = {a["id"] for r in (passes, fails) for at in r["attempts"] for a in at["attachments"] if a["name"] == "shared.bin"}
    assert len(shared_ids) >= 1
    for r in res.values():
        d = server.detail(key, r["id"])
        assert SECRET not in json.dumps(d)
        for a in d["attempts"]:
            assert a.get("traceId")
    trace = server.detail(key, res["sends traceparent to the backend"]["id"])["attempts"][0]
    seen = server.content(key, [a for a in trace["attachments"] if a["name"] == "seen-headers.json"][0]["id"])
    assert seen["traceparent"].split("-")[1] == trace["traceId"]
    assert "fastproof.attempt=0" in seen["baggage"]
    if BROWSER:
        b = server.detail(key, res["renders a page"]["id"])["attempts"][0]
        kinds = {a["kind"] for a in b["attachments"]}
        assert {"image", "video", "trace"} <= kinds, b["attachments"]


def test_server_down_run_passes_and_spool_is_flushed(server, tmp_path):
    key = server.project("pw-offline")
    dead = f"http://127.0.0.1:{free_port()}"
    out = run_pw(server, key, tmp_path, "--project", "api", "--grep", "passes with steps|skipped", url=dead)
    assert out.returncode == 0, out.stdout + out.stderr
    text = out.stdout + out.stderr
    assert "saved to" in text and "fastproof-playwright flush" in text, text
    assert server.launches(key) == []
    env = server.env(key, FASTPROOF_SPOOL_DIR=str(tmp_path / "spool"))
    flush = subprocess.run(
        ["node", "dist/cli.js", "flush"], cwd=str(PW), env=env, capture_output=True, text=True, timeout=120,
    )
    assert flush.returncode == 0, flush.stdout + flush.stderr
    assert "sent 2 results" in flush.stdout
    launch = wait_for(lambda: server.launches(key))[0]
    assert launch["status"] == "finished" and launch["counts"]["total"] == 2
    res = by_name(server, key, launch["id"])
    p = server.detail(key, res["passes with steps and output"]["id"])
    assert {a["name"] for a in p["attempts"][0]["attachments"]} >= {"note-****", "shared.bin"}


def test_testplan_grep_and_shard_join(server, tmp_path):
    key = server.project("pw-plan")
    plan = tmp_path / "testplan.json"
    plan.write_text(json.dumps({"version": "1.0", "tests": [{"selector": "api.spec.ts › checkout › skipped"}]}))
    extra = {"FASTPROOF_TESTPLAN": str(plan), "FASTPROOF_LAUNCH": "pipeline-42"}
    out = run_pw(server, key, tmp_path, "--project", "api", extra=extra)
    assert out.returncode == 0, out.stdout + out.stderr
    plan.write_text(json.dumps({"version": "1.0", "tests": [{"selector": "api.spec.ts#sends traceparent to the backend"}]}))
    out = run_pw(server, key, tmp_path, "--project", "api", extra=extra)
    assert out.returncode == 0, out.stdout + out.stderr
    launch = single_launch(server, key)
    assert launch["externalId"] == "pipeline-42"
    assert launch["status"] == "running"
    assert sorted(by_name(server, key, launch["id"])) == ["sends traceparent to the backend", "skipped"]
