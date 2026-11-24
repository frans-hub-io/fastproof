import json
import os

import pytest

from pytest_fastproof.ci import detect_ci
from pytest_fastproof.config import resolve_config
from pytest_fastproof.mask import MASK, Masker
from pytest_fastproof.plugin import attempt_status, result_id
from pytest_fastproof.spool import SpoolSession
from pytest_fastproof.testplan import TestPlan, allure_full_name, load_test_plan, strip_params
from pytest_fastproof.trace import TraceContext, format_baggage, parse_traceparent


def test_mask_env_values_patterns_and_short_values():
    m = Masker(
        values=["hunter2-explicit"],
        patterns=[r"card=\d{4}"],
        env={"API_TOKEN": "s3cr3t-value", "DB_PASSWORD": "p@ss word", "PLAIN": "visible", "FEATURE_TOKEN": "true", "X_KEY": "12"},
    )
    s = m.mask("t=s3cr3t-value p=p@ss word e=hunter2-explicit card=1234 v=visible f=true k=12 u=p%40ss%20word")
    assert s == f"t={MASK} p={MASK} e={MASK} {MASK} v=visible f=true k=12 u={MASK}"
    assert Masker(env_names=[], env={}).active is False
    assert Masker(env_names=["A", "B"], env={"A": "abcd", "B": "abcdefgh"}).mask("xabcdefghx") == f"x{MASK}x"


def test_trace_context():
    t = TraceContext.new("L", "tests/a.py::t", 1)
    assert parse_traceparent(t.traceparent) == (t.trace_id, t.span_id)
    assert t.headers()["traceparent"] == t.traceparent
    assert t.headers(new_span=True)["traceparent"] != t.traceparent
    assert t.env({"A": "1"}) == {"A": "1", "TRACEPARENT": t.traceparent, "BAGGAGE": t.baggage}
    assert format_baggage("a b", "x::y", 2) == "fastproof.launch=a%20b,fastproof.test=x%3A%3Ay,fastproof.attempt=2"
    assert parse_traceparent("00-" + "0" * 32 + "-" + "1" * 16 + "-01") is None
    assert TraceContext.new("L", "t", 0).trace_id != TraceContext.new("L", "t", 0).trace_id


def test_test_plan_forms(tmp_path):
    assert strip_params("tests/test_a.py::TestX::test_y[1-2]") == "tests/test_a.py::TestX::test_y"
    assert allure_full_name("tests/test_a.py::TestX::test_y[1]") == "tests.test_a.TestX#test_y"
    plan = TestPlan({"tests": [{"selector": "tests/test_a.py::test_one"}, {"selector": "tests.test_b#test_two"}, {"id": 7}]})
    assert plan.matches("tests/test_a.py::test_one[x]", [])
    assert plan.matches("tests/test_b.py::test_two", [])
    assert plan.matches("tests/test_c.py::test_z", ["7"])
    assert not plan.matches("tests/test_c.py::test_z", [])
    f = tmp_path / "testplan.json"
    f.write_text(json.dumps({"version": "1.0", "tests": [{"selector": "x"}]}))
    assert load_test_plan(env={"ALLURE_TESTPLAN_PATH": str(f)})["tests"][0]["selector"] == "x"
    assert load_test_plan(env={}) is None
    with pytest.raises(OSError):
        load_test_plan(env={"FASTPROOF_TESTPLAN": str(tmp_path / "missing.json")})


def test_ci_detection():
    assert detect_ci({"GITLAB_CI": "true", "CI_PROJECT_ID": "42", "CI_PIPELINE_ID": "1001"})["externalId"] == "gitlab-42-1001"
    gh = detect_ci({"GITHUB_ACTIONS": "true", "GITHUB_REPOSITORY": "o/r", "GITHUB_RUN_ID": "9", "GITHUB_RUN_ATTEMPT": "2"})
    assert gh["externalId"] == "github-o/r-9-2"
    assert detect_ci({"JENKINS_URL": "x", "JOB_NAME": "j", "BUILD_NUMBER": "5"})["ci"]["buildOrder"] == 5
    assert detect_ci({}) is None


def test_config_resolution(tmp_path):
    env = {"FASTPROOF_URL": "http://198.51.100.7", "FASTPROOF_PROJECT": "shop", "FASTPROOF_TOKEN": "tok-123456"}
    c = resolve_config({"launch": "ext-1"}, env, str(tmp_path))
    assert c.enabled and c.launch["externalId"] == "ext-1" and not c.own_launch
    assert c.spool_dir == os.path.join(str(tmp_path), ".fastproof", "spool")
    assert "tok-123456" in c.secret_values
    own = resolve_config({}, env, str(tmp_path))
    assert own.own_launch and own.launch["externalId"].startswith("local-")
    assert not resolve_config({}, {"FASTPROOF_PROJECT": "shop"}).enabled
    assert not resolve_config({}, dict(env, FASTPROOF_ENABLED="false")).enabled
    assert not resolve_config({}, dict(env, FASTPROOF_PROJECT="Bad Key")).enabled
    assert resolve_config({}, dict(env, FASTPROOF_FLUSH_TIMEOUT_MS="2500")).flush_timeout == 2.5
    assert "*TOKEN*" in resolve_config({"secret_env": ["MY_VAR"]}, env).secret_env


class Rep:
    def __init__(self, when, outcome, message=None, wasxfail=None):
        self.when = when
        self.outcome = outcome
        if message is not None:
            self.longrepr = (None, None, message) if outcome == "skipped" else message
        if wasxfail is not None:
            self.wasxfail = wasxfail


def test_attempt_status():
    ok = {"setup": Rep("setup", "passed"), "call": Rep("call", "passed"), "teardown": Rep("teardown", "passed")}
    assert attempt_status(ok)[0] == "passed"
    assert attempt_status(dict(ok, call=Rep("call", "failed", "assert 1 == 2")))[0] == "failed"
    assert attempt_status(dict(ok, call=Rep("call", "rerun", "AssertionError: x")))[0] == "failed"
    assert attempt_status(dict(ok, call=Rep("call", "failed", "RuntimeError: boom")))[0] == "broken"
    assert attempt_status({"setup": Rep("setup", "failed", "E")})[0] == "broken"
    assert attempt_status({"setup": Rep("setup", "skipped", "Skipped: no")})[0] == "skipped"
    assert attempt_status(dict(ok, call=Rep("call", "skipped", "x", wasxfail="bug")))[0] == "skipped"
    assert attempt_status(dict(ok, teardown=Rep("teardown", "failed", "E")))[0] == "broken"
    assert attempt_status({"???": Rep("???", "failed", "worker crashed")})[0] == "broken"
    assert attempt_status({})[0] == "unknown"
    assert result_id("r", "a::b", 1) != result_id("r", "a::b", 0)


def test_spool_budget_keeps_room_for_failures(tmp_path):
    meta = {"v": 1, "reporter": "t", "project": "demo", "launch": {"externalId": "e", "name": "n"}, "pid": 1, "host": "h"}
    s = SpoolSession(str(tmp_path), "s1", meta, 1000)
    assert not s.add_blob("a" * 64, 600, True, body=b"\0" * 600)
    assert s.add_blob("b" * 64, 600, False, body=b"\0" * 600)
    assert not s.add_blob("c" * 64, 600, False, body=b"\0" * 600)
    assert s.append({"v": 1, "result": {"id": "z", "name": "z", "status": "passed"}, "blobs": []})
    assert s.dropped["blobs"] == 2
    assert '"z"' in (tmp_path / "s1" / "results.ndjson").read_text()


def _plant_session(spool, sid, project, blobs):
    d = spool / sid
    (d / "blobs").mkdir(parents=True)
    (d / "meta.json").write_text(json.dumps({
        "v": 1, "reporter": "t", "project": project, "launch": {"externalId": sid, "name": sid},
        "createdAt": "", "pid": 1, "host": "other", "ended": True,
    }))
    atts = [{"id": "blob:" + b["sha256"], "name": b["name"]} for b in blobs]
    result = {"id": "r-" + sid, "name": "x", "fullName": "x", "status": "passed", "start": 1, "stop": 2, "retry": 0,
              "attachments": atts}
    entry = {"v": 1, "result": result, "blobs": [dict(b, contentType="text/plain", size=1) for b in blobs]}
    (d / "results.ndjson").write_text(json.dumps(entry) + "\n")
    return d / "blobs"


def test_flush_sends_only_blobs_inside_the_session_that_match_their_name(tmp_path):
    import hashlib
    from fake_server import FakeServer
    from pytest_fastproof.api import Api
    from pytest_fastproof.spool import flush_spool

    secret = tmp_path / "secret.txt"
    secret.write_text("TOKEN=do-not-send")
    spool = tmp_path / "spool"
    good = b"good attachment"
    good_sha = hashlib.sha256(good).hexdigest()
    linked, wrong = "a" * 64, "b" * 64
    blobs = _plant_session(spool, "s1", "demo", [
        {"sha256": str(secret), "name": "absolute"},
        {"sha256": "../../secret.txt", "name": "traversal"},
        {"sha256": linked, "name": "link"},
        {"sha256": wrong, "name": "wrong"},
        {"sha256": good_sha, "name": "good"},
    ])
    os.symlink(secret, blobs / linked)
    (blobs / wrong).write_text("not what the name says")
    (blobs / good_sha).write_bytes(good)
    _plant_session(spool, "s2", "demo/launches?x=", [])
    with FakeServer(token="t") as fake:
        out = flush_spool(Api(fake.url, "t", "demo"), str(spool))
        assert fake.uploads == 1
        assert [a["body"] for a in fake.attachments.values()] == [good]
        assert out["results"] == 1
        assert any("invalid project key" in e for e in out["errors"])
    assert (spool / "s2").exists()
