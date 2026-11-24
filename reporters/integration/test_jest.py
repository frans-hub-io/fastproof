import json
import shutil
import subprocess

import pytest

from conftest import REPORTERS, by_name, single_launch

JT = REPORTERS / "jest"
SECRET = "jt-integration-secret-55"

pytestmark = pytest.mark.skipif(
    not (JT / "node_modules" / "jest").is_dir() or not (JT / "dist" / "index.js").is_file(),
    reason="run `npm install --legacy-peer-deps && npm run build` in reporters/jest first",
)


def test_jest_live_run_attempts_logs_steps_attachments_and_masking(server, tmp_path):
    key = server.project("jt-live")
    (tmp_path / "state").mkdir()
    env = server.env(
        key,
        E2E_SECRET_TOKEN=SECRET,
        E2E_STATE_DIR=str(tmp_path / "state"),
        FASTPROOF_SPOOL_DIR=str(tmp_path / "spool"),
        FASTPROOF_LAUNCH="ci-jt-1",
        FASTPROOF_FLUSH_TIMEOUT_MS="10000",
    )
    out = subprocess.run(
        [shutil.which("npx") or "npx", "jest", "-c", "e2e/jest.config.js", "--maxWorkers=2"],
        cwd=str(JT), env=env, capture_output=True, text=True, timeout=600,
    )
    assert out.returncode == 1, out.stdout + out.stderr
    assert "[fastproof] launch #1:" in out.stdout + out.stderr, out.stdout + out.stderr
    launch = single_launch(server, key)
    assert launch["status"] in ("running", "finished")
    res = by_name(server, key, launch["id"])
    flaky = server.detail(key, res["flaky: passes on third try"]["id"])
    assert [a["status"] for a in flaky["attempts"]] == ["failed", "failed", "passed"]
    assert len({a["traceId"] for a in flaky["attempts"]}) == 3
    assert [a["status"] for a in server.detail(key, res["fails every time"]["id"])["attempts"]] == ["failed"] * 3
    assert server.detail(key, res["throws a plain error"]["id"])["attempts"][0]["status"] == "broken"
    assert res["skipped"]["status"] == "skipped"
    assert "tests/broken.test.js (suite failed to run)" in res
    passes = server.detail(key, res["passes with output [fastproof-id:CASE-1]"]["id"])
    assert "starting with token ****" in passes["attempts"][0]["stdout"]
    assert "inner step" in json.dumps(
        server.get(
            f"/api/v1/projects/{key}/launches/{launch['id']}/logs?resultId={res['passes with output [fastproof-id:CASE-1]']['id']}"
        )["items"]
    )
    for r in res.values():
        assert SECRET not in json.dumps(server.detail(key, r["id"]))
    note = [a for a in flaky["attempts"][-1]["attachments"] if a["name"] == "note"][0]
    assert server.content(key, note["id"]) == b"body with ****"
    t = server.detail(key, res["sends traceparent"]["id"])["attempts"][0]
    seen = server.content(key, [a for a in t["attachments"] if a["name"] == "seen-headers.json"][0]["id"])
    assert seen["traceparent"].split("-")[1] == t["traceId"]
