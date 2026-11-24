import json
import shutil
import subprocess

import pytest

from conftest import REPORTERS, by_name, single_launch

VT = REPORTERS / "vitest"
SECRET = "vt-integration-secret-55"

pytestmark = pytest.mark.skipif(
    not (VT / "node_modules" / "vitest").is_dir() or not (VT / "dist" / "index.js").is_file(),
    reason="run `npm install --legacy-peer-deps && npm run build` in reporters/vitest first",
)


def test_vitest_live_run_attempts_logs_attachments_and_masking(server, tmp_path):
    key = server.project("vt-live")
    (tmp_path / "state").mkdir()
    env = server.env(
        key,
        E2E_SECRET_TOKEN=SECRET,
        E2E_STATE_DIR=str(tmp_path / "state"),
        FASTPROOF_SPOOL_DIR=str(tmp_path / "spool"),
        FASTPROOF_LAUNCH="ci-vt-1",
        FASTPROOF_FLUSH_TIMEOUT_MS="10000",
    )
    out = subprocess.run(
        [shutil.which("npx") or "npx", "vitest", "run", "--root", "e2e"],
        cwd=str(VT), env=env, capture_output=True, text=True, timeout=600,
    )
    assert out.returncode == 1, out.stdout + out.stderr
    assert "[fastproof] launch #1: 12 results" in out.stdout + out.stderr, out.stdout + out.stderr
    launch = single_launch(server, key)
    res = by_name(server, key, launch["id"])
    assert launch["counts"]["flaky"] == 1
    flaky = server.detail(key, res["flaky: passes on third try"]["id"])
    assert [a["status"] for a in flaky["attempts"]] == ["failed", "failed", "passed"]
    assert len({a["traceId"] for a in flaky["attempts"]}) == 3
    assert flaky["flakyInRun"] is True
    assert [a["status"] for a in server.detail(key, res["fails every time"]["id"])["attempts"]] == ["failed", "failed"]
    assert server.detail(key, res["throws a plain error"]["id"])["attempts"][0]["status"] == "broken"
    passes = server.detail(key, res["passes with output"]["id"])
    assert "starting with token ****" in passes["attempts"][0]["stdout"]
    for r in res.values():
        assert SECRET not in json.dumps(server.detail(key, r["id"]))
    note = [a for a in flaky["attempts"][-1]["attachments"] if a["name"] == "note"][0]
    assert server.content(key, note["id"]) == b"body with ****"
    t = server.detail(key, res["sends traceparent"]["id"])["attempts"][0]
    seen = server.content(key, [a for a in t["attachments"] if a["name"] == "seen-headers.json"][0]["id"])
    assert seen["traceparent"].split("-")[1] == t["traceId"]
    big = server.detail(key, res["big attachment"]["id"])["attempts"][0]["attachments"][0]
    assert len(server.content(key, big["id"])) == 2 * 1024 * 1024
    lines = server.get(
        f"/api/v1/projects/{key}/launches/{launch['id']}/logs?resultId={res['flaky: passes on third try']['id']}"
    )["items"]
    assert [ln["message"] for ln in lines if "flaky attempt" in ln["message"]] == [
        "flaky attempt 0", "flaky attempt 1", "flaky attempt 2"
    ]
