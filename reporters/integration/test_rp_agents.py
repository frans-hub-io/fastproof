"""The official ReportPortal agents (pytest-reportportal, agent-js-playwright) against the real server.

Set ``FASTPROOF_TEST_RP_AGENTS`` to a directory holding the installed agents: ``py/`` (a pip ``--target``
directory with pytest, pytest-reportportal, pytest-rerunfailures and pytest-xdist) and ``node_modules/`` of
``crates/fastproof-rp/tests/recorder/scenarios/playwright``. Otherwise the tests are skipped.
"""

import hashlib
import os
import shutil
import subprocess
import sys
import urllib.request

import pytest

from conftest import REPORTERS, by_name, single_launch, wait_for

AGENTS = os.environ.get("FASTPROOF_TEST_RP_AGENTS")
SCENARIOS = REPORTERS.parent / "crates" / "fastproof-rp" / "tests" / "recorder" / "scenarios"
KEY = "rp-e2e"

pytestmark = pytest.mark.skipif(not AGENTS, reason="FASTPROOF_TEST_RP_AGENTS is not set")


def uploader(server, key):
    body = server.call("POST", "/api/v1/tokens", {"name": f"rp-{key}", "role": "uploader", "scopes": [key], "expiresInDays": 1})
    return body["token"]


def settled(server, key, launch_id, total):
    def done():
        results = server.results(key, launch_id)
        return len(results) >= total and all(r["status"] != "running" for r in results) and results

    return wait_for(done, 60)


def finished_launch(server, key, number):
    def done():
        for item in server.launches(key):
            if item["number"] == number and item["status"] != "running":
                return item
        return None

    return wait_for(done, 60)


@pytest.fixture
def project(server, request):
    key = f"{KEY}-{hashlib.sha1(request.node.name.encode()).hexdigest()[:8]}"
    server.project(key)
    return key


def run_pytest(server, key, tmp_path, token, *args):
    work = tmp_path / "pytest"
    shutil.copytree(SCENARIOS / "pytest", work)
    env = {
        "PATH": os.environ.get("PATH", ""),
        "HOME": str(tmp_path),
        "PYTHONPATH": str(AGENTS + "/py"),
    }
    return subprocess.run(
        [
            sys.executable, "-m", "pytest", "-p", "no:cacheprovider", "--reportportal", "-q",
            "-o", f"rp_endpoint={server.url}/rp", "-o", f"rp_project={key}", "-o", f"rp_api_key={token}",
            *args,
        ],
        cwd=str(work), env=env, capture_output=True, text=True, timeout=300,
    )


def test_pytest_agent_reports_into_native_results(server, project, tmp_path):
    token = uploader(server, project)
    run_pytest(server, project, tmp_path, token)
    launch = finished_launch(server, project, 1)
    assert launch, "the launch did not finish"
    results = settled(server, project, launch["id"], 19)
    assert len(results) == 19, [r["name"] for r in results]
    failed = [r for r in results if r["status"] == "failed"]
    assert len(failed) >= 6
    assert all(r.get("message") for r in failed), [r["name"] for r in failed if not r.get("message")]
    by = {r["name"]: r for r in results}
    flaky = server.detail(project, by["tests/test_basic.py::test_flaky"]["id"])
    assert flaky["attempts"][-1]["status"] == "passed"
    defect = server.detail(project, by["tests/test_basic.py::test_known_defect"]["id"])
    assert any(lb["name"] == "rp.issue" for lb in defect["labels"]), defect["labels"]
    assert any(link["type"] == "issue" for link in defect.get("links", [])), defect.get("links")
    att = server.detail(project, by["tests/test_logs.py::test_attachment"]["id"])["attempts"][-1]["attachments"]
    assert {a["name"] for a in att} >= {"note.txt", "pixel.png", "blob.bin"}, att
    note = [a for a in att if a["name"] == "note.txt"][0]
    assert server.content(project, note["id"]) == b"hello attachment\n"


def test_pytest_xdist_joins_a_native_launch(server, project, tmp_path):
    token = uploader(server, project)
    native = server.call("POST", f"/api/v1/projects/{project}/launches", {"name": "ci", "externalId": "ci-rp-join"})
    run_pytest(server, project, tmp_path, token, "-n", "2", "-o", f"rp_launch_uuid={native['id']}")
    results = settled(server, project, native["id"], 19)
    assert len(results) == 19
    server.call("POST", f"/api/v1/projects/{project}/launches/{native['id']}/finish")
    assert len(server.launches(project)) == 1


def run_playwright(server, key, tmp_path, token, *args, env_extra=None):
    work = tmp_path / "playwright"
    if not work.exists():
        shutil.copytree(SCENARIOS / "playwright", work, ignore=shutil.ignore_patterns("node_modules"))
        os.symlink(AGENTS + "/node_modules", work / "node_modules")
    env = {
        k: v for k, v in os.environ.items() if k in ("PATH", "HOME", "LD_LIBRARY_PATH", "PLAYWRIGHT_BROWSERS_PATH")
    }
    env.update({"RP_ENDPOINT": f"{server.url}/rp", "RP_PROJECT": key, "RP_API_KEY": token, **(env_extra or {})})
    return subprocess.run(
        ["npx", "playwright", "test", *args], cwd=str(work), env=env, capture_output=True, text=True, timeout=600
    )


@pytest.mark.parametrize("version", ["v2", "v1"])
def test_playwright_agent_reports_retries_steps_and_files(server, project, tmp_path, version):
    token = uploader(server, project)
    run_playwright(server, project, tmp_path, token, env_extra={"RP_API_VERSION": version})
    launch = finished_launch(server, project, 1)
    assert launch, "the launch did not finish"
    results = settled(server, project, launch["id"], 11)
    assert len(results) == 11, [r["name"] for r in results]
    by = {r["name"]: r for r in results}
    retried = server.detail(project, by["fails on first attempt then passes"]["id"])
    assert [a["status"] for a in retried["attempts"]] == ["failed", "passed"], retried["attempts"]
    always = server.detail(project, by["always fails"]["id"])
    assert [a["status"] for a in always["attempts"]] == ["failed", "failed"]
    assert by["always fails"].get("message")
    steps = server.detail(project, by["passes with steps"]["id"])["attempts"][-1]["steps"]
    names = [st["name"] for st in steps]
    assert "open page" in names and "check heading" in names, names
    check = [st for st in steps if st["name"] == "check heading"][0]
    assert [st["name"] for st in check["steps"]] == ["inner step"], check
    files = server.detail(project, by["always fails"]["id"])["attempts"][-1]["attachments"]
    assert "image" in {a["kind"] for a in files}, files
    skipped = [r for r in results if r["status"] == "skipped"]
    assert len(skipped) == 1


def test_playwright_shards_join_one_native_launch(server, project, tmp_path):
    token = uploader(server, project)
    native = server.call("POST", f"/api/v1/projects/{project}/launches", {"name": "ci", "externalId": "ci-shards"})
    for shard in ("1/2", "2/2"):
        run_playwright(
            server, project, tmp_path, token, f"--shard={shard}", env_extra={"RP_LAUNCH_ID": native["id"]}
        )
    results = settled(server, project, native["id"], 11)
    assert len(results) == 11
    assert len(server.launches(project)) == 1


@pytest.mark.xfail(
    strict=True,
    reason="the agent names the file <test>_trace with content type application/zip; the kind inference needs an extension",
)
def test_playwright_trace_is_recognized_as_a_trace(server, project, tmp_path):
    token = uploader(server, project)
    run_playwright(server, project, tmp_path, token, "--grep", "always fails")
    launch = finished_launch(server, project, 1)
    results = settled(server, project, launch["id"], 1)
    files = server.detail(project, results[0]["id"])["attempts"][-1]["attachments"]
    assert "trace" in {a["kind"] for a in files}, files
