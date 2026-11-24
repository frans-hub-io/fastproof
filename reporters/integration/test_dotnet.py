import json
import os
import subprocess

import pytest

from conftest import REPORTERS, by_name, single_launch

DN = REPORTERS / "dotnet"
SECRET = "dotnet-secret-7788"
DOTNET = os.environ.get("FASTPROOF_TEST_DOTNET")

pytestmark = pytest.mark.skipif(not DOTNET, reason="FASTPROOF_TEST_DOTNET (path to the dotnet launcher) is not set")


def run_sample(server, key, tmp_path, sample, framework="net8.0", extra_env=None, args=(), timeout=900):
    state = tmp_path / "state"
    state.mkdir(exist_ok=True)
    env = server.env(
        key,
        SAMPLE_API_TOKEN=SECRET,
        SAMPLE_STATE_DIR=str(state),
        FASTPROOF_SPOOL_DIR=str(tmp_path / "spool"),
        FASTPROOF_FLUSH_TIMEOUT_MS="10000",
        DOTNET_CLI_TELEMETRY_OPTOUT="1",
        DOTNET_NOLOGO="1",
    )
    env.update(extra_env or {})
    return subprocess.run(
        [DOTNET, "test", "-f", framework, *args],
        cwd=str(DN / "samples" / sample), env=env, capture_output=True, text=True, timeout=timeout,
    )


def logs_of(server, key, launch, result):
    return server.get(f"/api/v1/projects/{key}/launches/{launch['id']}/logs?resultId={result['id']}")["items"]


def seen_headers(server, key, detail):
    att = detail["attempts"][0]
    return server.content(key, [a for a in att["attachments"] if a["name"] == "seen-headers.json"][0]["id"])


@pytest.mark.parametrize("framework", ["net8.0", "net10.0"])
def test_nunit_attempts_parameters_logs_attachments_and_masking(server, tmp_path, framework):
    key = server.project(f"dn-nunit-{framework.replace('.', '')}")
    out = run_sample(server, key, tmp_path, "nunit", framework)
    assert out.returncode == 1, out.stdout[-3000:] + out.stderr[-2000:]
    assert "[fastproof] launch #1:" in out.stdout + out.stderr
    launch = single_launch(server, key)
    assert launch["status"] == "finished"
    res = by_name(server, key, launch["id"])
    flaky = server.detail(key, res["FlakyPassesOnThirdTry"]["id"])
    assert [a["status"] for a in flaky["attempts"]] == ["failed", "failed", "passed"]
    assert flaky["flakyInRun"] is True
    assert len({a["traceId"] for a in flaky["attempts"]}) == 3
    assert [a["status"] for a in server.detail(key, res["FailsEveryTime"]["id"])["attempts"]] == ["failed", "failed"]
    assert server.detail(key, res["ThrowsAPlainError"]["id"])["attempts"][0]["status"] == "broken"
    assert res["Skipped"]["status"] == "skipped"
    rows = {n: r for n, r in res.items() if n.startswith("AddsNumbers(")}
    assert len(rows) == 3
    assert sorted(r["status"] for r in rows.values()) == ["failed", "passed", "passed"]
    p = server.detail(key, res["PassesWithStepsLogsAndAttachments"]["id"])
    att = p["attempts"][0]
    assert att["status"] == "passed"
    assert "starting with token ****" in att["stdout"]
    note = [a for a in att["attachments"] if a["name"].startswith("note-")][0]
    assert note["name"] == "note-****.txt"
    assert server.content(key, note["id"]) == b"body with ****"
    flat = json.dumps(att["steps"])
    assert "outer" in flat and "inner" in flat
    assert any(lb["name"] == "tag" and lb["value"] == "checkout" for lb in p["labels"])
    assert any(lk["type"] == "tms" for lk in p["links"])
    lines = logs_of(server, key, launch, res["PassesWithStepsLogsAndAttachments"])
    assert any(ln["stream"] == "step" and "inner" in ln["message"] for ln in lines), lines
    assert any(ln["stream"] == "stdout" and "inside the inner step" in ln["message"] for ln in lines)
    assert any(ln["stream"] == "log" and ln["level"] == "warn" for ln in lines)
    fails = server.detail(key, res["FailsAssertion"]["id"])["attempts"][0]
    assert "Expected: 5" in fails["error"]["message"]
    assert "about to fail" in fails["stderr"]
    trace_ids = set()
    for r in res.values():
        d = server.detail(key, r["id"])
        assert SECRET not in json.dumps(d)
        for a in d["attempts"]:
            assert a.get("traceId"), r["name"]
            trace_ids.add(a["traceId"])
    assert len(trace_ids) == sum(r["attemptCount"] for r in res.values())
    t = server.detail(key, res["TraceparentReachesTheBackend"]["id"])
    assert seen_headers(server, key, t)["traceparent"].split("-")[1] == t["attempts"][0]["traceId"]


def test_nunit_server_down_spools_and_the_next_run_sends_it(server, tmp_path):
    key = server.project("dn-nunit-spool")
    down = run_sample(
        server, key, tmp_path, "nunit",
        extra_env={"FASTPROOF_URL": "http://127.0.0.1:9", "FASTPROOF_FLUSH_TIMEOUT_MS": "3000", "FASTPROOF_LAUNCH": "ci-dn-spool"},
    )
    assert down.returncode == 1, down.stdout[-3000:] + down.stderr[-2000:]
    assert "saved to" in down.stdout + down.stderr
    sessions = [p for p in (tmp_path / "spool").iterdir() if p.is_dir()]
    assert len(sessions) == 1
    meta = json.loads((sessions[0] / "meta.json").read_text())
    assert meta["v"] == 1 and meta["ended"] is True and meta["project"] == key
    assert server.token not in json.dumps(meta)
    assert server.launches(key) == []
    up = run_sample(
        server, key, tmp_path, "nunit",
        extra_env={"FASTPROOF_LAUNCH": "ci-dn-spool"}, args=("--filter", "Name=Waits"),
    )
    assert up.returncode == 0, up.stdout[-3000:] + up.stderr[-2000:]
    launch = single_launch(server, key)
    res = by_name(server, key, launch["id"])
    assert "FlakyPassesOnThirdTry" in res and "Waits" in res
    flaky = server.detail(key, res["FlakyPassesOnThirdTry"]["id"])
    assert [a["status"] for a in flaky["attempts"]] == ["failed", "failed", "passed"]
    assert not [p for p in (tmp_path / "spool").iterdir() if p.is_dir()]


def test_nunit_test_plan_runs_only_the_selected_tests(server, tmp_path):
    key = server.project("dn-nunit-plan")
    plan = tmp_path / "testplan.json"
    plan.write_text(json.dumps({"version": "1.0", "tests": [{"selector": "Samples.CheckoutTests.AddsNumbers"}, {"id": "CASE-1"}]}))
    run_sample(server, key, tmp_path, "nunit", extra_env={"FASTPROOF_TESTPLAN": str(plan)})
    res = by_name(server, key, single_launch(server, key)["id"])
    assert {n for n in res if not n.startswith("AddsNumbers(")} == {"PassesWithStepsLogsAndAttachments"}
    assert len([n for n in res if n.startswith("AddsNumbers(")]) == 3


@pytest.mark.parametrize("framework", ["net8.0", "net10.0"])
def test_xunit_theories_output_logs_attachments_and_masking(server, tmp_path, framework):
    key = server.project(f"dn-xunit-{framework.replace('.', '')}")
    out = run_sample(server, key, tmp_path, "xunit", framework)
    assert out.returncode != 0, out.stdout[-3000:] + out.stderr[-2000:]
    assert "[fastproof] launch #1:" in out.stdout + out.stderr
    launch = single_launch(server, key)
    assert launch["status"] == "finished"
    res = by_name(server, key, launch["id"])
    rows = {n: r for n, r in res.items() if n.startswith("AddsNumbers(")}
    assert len(rows) == 3
    assert sorted(r["status"] for r in rows.values()) == ["failed", "passed", "passed"]
    assert server.detail(key, res["ThrowsAPlainError"]["id"])["attempts"][0]["status"] == "broken"
    assert res["SkipsAtRunTime"]["status"] == "skipped"
    assert res["FailsAssertion"]["status"] == "failed"
    p = server.detail(key, res["PassesWithStepsLogsAndAttachments"]["id"])
    att = p["attempts"][0]
    assert "starting with token ****" in att["stdout"] and "console line" in att["stdout"]
    note = [a for a in att["attachments"] if a["name"].startswith("note-")][0]
    assert note["name"] == "note-****.txt"
    assert server.content(key, note["id"]) == b"body with ****"
    assert "inner" in json.dumps(att["steps"])
    assert any(lb["name"] == "tag" and lb["value"] == "checkout" for lb in p["labels"])
    lines = logs_of(server, key, launch, res["PassesWithStepsLogsAndAttachments"])
    assert any(ln["stream"] == "stdout" and "inside the inner step" in ln["message"] for ln in lines), lines
    for r in res.values():
        d = server.detail(key, r["id"])
        assert SECRET not in json.dumps(d)
        assert all(a.get("traceId") for a in d["attempts"])
    t = server.detail(key, res["TraceparentReachesTheBackend"]["id"])
    assert seen_headers(server, key, t)["traceparent"].split("-")[1] == t["attempts"][0]["traceId"]
