import json
import shutil
import subprocess

import pytest

from conftest import REPORTERS, by_name, single_launch

AJ = REPORTERS / "allure-js"
SECRET = "allure-js-secret-4242"

pytestmark = pytest.mark.skipif(
    not (AJ / "node_modules" / "allure-playwright").is_dir() or not (AJ / "dist" / "playwright.js").is_file(),
    reason="run `npm install && npm run build` in reporters/allure-js first",
)


def test_allure_playwright_streams_live_and_keeps_allure_results(server, tmp_path):
    key = server.project("aj-live")
    env = server.env(
        key,
        E2E_SECRET_TOKEN=SECRET,
        E2E_OUTPUT_DIR=str(tmp_path / "out"),
        E2E_RESULTS_DIR=str(tmp_path / "allure-results"),
        FASTPROOF_SPOOL_DIR=str(tmp_path / "spool"),
        FASTPROOF_FLUSH_TIMEOUT_MS="8000",
    )
    out = subprocess.run(
        [shutil.which("npx") or "npx", "playwright", "test", "-c", "e2e"],
        cwd=str(AJ), env=env, capture_output=True, text=True, timeout=600,
    )
    assert out.returncode == 1, out.stdout + out.stderr
    assert "[fastproof] launch #1: 7 results" in out.stdout + out.stderr, out.stdout + out.stderr
    launch = single_launch(server, key)
    assert launch["status"] == "finished"
    c = launch["counts"]
    assert (c["total"], c["failed"], c["skipped"], c["flaky"]) == (5, 1, 1, 1)
    res = by_name(server, key, launch["id"])
    assert sum(r["attemptCount"] for r in res.values()) == 7
    flaky = server.detail(key, res["flaky: passes on retry"]["id"])
    assert [a["status"] for a in flaky["attempts"]] == ["failed", "passed"]
    assert flaky["attempts"][0]["traceId"] != flaky["attempts"][1]["traceId"]
    assert flaky["flakyInRun"] is True
    fails = server.detail(key, res["fails every time"]["id"])
    assert [a["status"] for a in fails["attempts"]] == ["failed", "failed"]
    passes = server.detail(key, res["passes with steps and output"]["id"])
    att = passes["attempts"][0]
    note = [a for a in att["attachments"] if a["name"].startswith("note-")][0]
    assert note["name"] == "note-****.txt"
    assert server.content(key, note["id"]) == b"body with ****"
    flat = json.dumps(att["steps"])
    assert "outer step" in flat and "inner step" in flat
    lines = server.get(
        f"/api/v1/projects/{key}/launches/{launch['id']}/logs?resultId={res['passes with steps and output']['id']}"
    )["items"]
    assert any(ln["stream"] == "step" and "inner step" in ln["message"] for ln in lines), lines
    for r in res.values():
        d = server.detail(key, r["id"])
        assert SECRET not in json.dumps(d)
        for a in d["attempts"]:
            assert a.get("traceId"), r["name"]
    t = server.detail(key, res["sends traceparent to the backend"]["id"])["attempts"][0]
    seen = server.content(key, [a for a in t["attachments"] if a["name"] == "seen-headers.json"][0]["id"])
    assert seen["traceparent"].split("-")[1] == t["traceId"]
    written = list((tmp_path / "allure-results").glob("*-result.json"))
    assert len(written) == 7
