import json
import shutil
import subprocess

import pytest

from conftest import REPORTERS, by_name, single_launch

CY = REPORTERS / "cypress"
SECRET = "cy-integration-secret-55"

pytestmark = pytest.mark.skipif(
    not (CY / "dist" / "plugin.js").is_file(),
    reason="run `npm install --legacy-peer-deps && npm run build` in reporters/cypress first",
)


def test_cypress_plugin_events_against_a_real_server(server, tmp_path):
    key = server.project("cy-live")
    env = server.env(
        key,
        E2E_SECRET_TOKEN=SECRET,
        FASTPROOF_SPOOL_DIR=str(tmp_path / "spool"),
        FASTPROOF_LAUNCH="ci-cy-1",
    )
    out = subprocess.run(
        [shutil.which("node") or "node", "e2e/drive.js"], cwd=str(CY), env=env, capture_output=True, text=True, timeout=120
    )
    assert out.returncode == 0, out.stdout + out.stderr
    traces = json.loads(out.stdout.strip().splitlines()[-1])
    assert "[fastproof] launch #1: 4 results" in out.stderr + out.stdout
    launch = single_launch(server, key)
    res = by_name(server, key, launch["id"])
    flaky = server.detail(key, res["flaky"]["id"])
    assert [a["status"] for a in flaky["attempts"]] == ["failed", "passed"]
    assert [a["traceId"] for a in flaky["attempts"]] == [traces["first"], traces["second"]]
    assert server.detail(key, res["broken one"]["id"])["attempts"][0]["status"] == "broken"
    assert res["later"]["status"] == "skipped"
    names = [a["name"] for a in flaky["attempts"][0]["attachments"]]
    assert sorted(names) == ["cart -- flaky (failed)", "video.mp4"]
    assert not flaky["attempts"][1]["attachments"]
    video = [a for a in flaky["attempts"][0]["attachments"] if a["name"] == "video.mp4"][0]
    assert len(server.content(key, video["id"])) == 2 * 1024 * 1024
    for r in res.values():
        assert SECRET not in json.dumps(server.detail(key, r["id"]))
    lines = server.get(f"/api/v1/projects/{key}/launches/{launch['id']}/logs?resultId={res['flaky']['id']}")["items"]
    assert any(ln["message"] == "visit /cart ****" and ln["attempt"] == 0 for ln in lines), lines
