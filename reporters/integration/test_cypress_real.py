import json
import os
import shutil
import subprocess

import pytest

from conftest import REPORTERS, by_name, single_launch

CY = REPORTERS / "cypress"

pytestmark = pytest.mark.skipif(
    os.environ.get("FASTPROOF_TEST_CYPRESS") != "1" or not (CY / "dist" / "plugin.js").is_file(),
    reason="set FASTPROOF_TEST_CYPRESS=1 and build reporters/cypress first",
)


def test_real_cypress_run(server, tmp_path):
    key = server.project("cy-real")
    env = server.env(key, FASTPROOF_SPOOL_DIR=str(tmp_path / "spool"), FASTPROOF_LAUNCH="ci-cy-real")
    out = subprocess.run(
        [shutil.which("npx") or "npx", "cypress", "run", "--project", "sample"],
        cwd=str(CY), env=env, capture_output=True, text=True, timeout=900,
    )
    assert out.returncode == 1, out.stdout + out.stderr
    assert "[fastproof] launch #1:" in out.stdout + out.stderr, out.stdout + out.stderr
    launch = single_launch(server, key)
    res = by_name(server, key, launch["id"])
    flaky = server.detail(key, res["flaky: passes on third try"]["id"])
    assert [a["status"] for a in flaky["attempts"]] == ["failed", "failed", "passed"], json.dumps(flaky["attempts"])
    assert len({a["traceId"] for a in flaky["attempts"]}) == 3
    fails = server.detail(key, res["fails every time"]["id"])
    assert [a["status"] for a in fails["attempts"]] == ["failed"] * 3
    assert fails["attempts"][-1]["attachments"], "a failed attempt should carry its screenshot or the video"
    passes = server.detail(key, res["passes and logs [fastproof-id:CASE-1]"]["id"])
    assert passes["attempts"][0]["status"] == "passed"
    assert res["skipped"]["status"] == "skipped"
