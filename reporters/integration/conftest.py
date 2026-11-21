"""Integration tests of both reporters against a real ``fastproof`` server binary.

Set ``FASTPROOF_TEST_SERVER_BIN`` (path to the built binary) and ``FASTPROOF_TEST_DATABASE_URL`` (a PostgreSQL
URL with trust authentication that may create databases); otherwise the tests are skipped.
"""

import json
import os
import secrets
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from urllib.parse import urlparse, urlunparse

import pytest

sys.path.insert(0, str(Path(__file__).parent))

import pgmini  # noqa: E402

REPORTERS = Path(__file__).resolve().parent.parent
TOKEN = "integration-admin-token-" + secrets.token_hex(8)


def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


class Server:
    def __init__(self, url: str, token: str, workdir: Path) -> None:
        self.url = url
        self.token = token
        self.workdir = workdir

    def call(self, method: str, path: str, body=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(
            self.url + path, data=data, method=method,
            headers={"Authorization": f"Bearer {self.token}", "Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=30) as r:
            raw = r.read()
        return json.loads(raw) if raw[:1] in (b"{", b"[") else raw

    def get(self, path: str):
        return self.call("GET", path)

    def project(self, key: str) -> str:
        try:
            self.call("POST", "/api/v1/projects", {"key": key, "name": key})
        except urllib.error.HTTPError as e:
            if e.code != 409:
                raise
        return key

    def launches(self, key: str):
        return self.get(f"/api/v1/projects/{key}/launches?limit=50")["items"]

    def results(self, key: str, launch_id: str):
        return self.get(f"/api/v1/projects/{key}/launches/{launch_id}/results?limit=500")["items"]

    def detail(self, key: str, result_id: str):
        return self.get(f"/api/v1/projects/{key}/results/{result_id}")

    def content(self, key: str, attachment_id: str) -> bytes:
        return self.get(f"/api/v1/projects/{key}/attachments/{attachment_id}/content")

    def env(self, project: str, **extra) -> dict:
        env = {k: v for k, v in os.environ.items() if not k.startswith("FASTPROOF_")}
        for k in ("GITLAB_CI", "GITHUB_ACTIONS", "JENKINS_URL"):
            env.pop(k, None)
        env.update({"FASTPROOF_URL": self.url, "FASTPROOF_TOKEN": self.token, "FASTPROOF_PROJECT": project})
        env.update(extra)
        return env


@pytest.fixture(scope="session")
def server(tmp_path_factory):
    binary = os.environ.get("FASTPROOF_TEST_SERVER_BIN")
    db_url = os.environ.get("FASTPROOF_TEST_DATABASE_URL")
    if not binary or not db_url:
        pytest.skip("FASTPROOF_TEST_SERVER_BIN and FASTPROOF_TEST_DATABASE_URL are not set")
    name = f"fastproof_test_reporters_{secrets.token_hex(4)}"
    pgmini.execute(db_url, f"create database {name}")
    u = urlparse(db_url)
    url = urlunparse(u._replace(path=f"/{name}"))
    work = tmp_path_factory.mktemp("server")
    tokens = work / "tokens"
    tokens.write_text(f"admin:admin:{TOKEN}\n")
    port, ops = free_port(), free_port()
    log = open(work / "server.log", "wb")
    proc = subprocess.Popen(
        [binary, "serve"],
        env={
            "PATH": os.environ.get("PATH", ""),
            "FASTPROOF_DATABASE_URL": url,
            "FASTPROOF_LISTEN": f"127.0.0.1:{port}",
            "FASTPROOF_OPS_LISTEN": f"127.0.0.1:{ops}",
            "FASTPROOF_BLOB_DIR": str(work / "blobs"),
            "FASTPROOF_SPOOL_DIR": str(work / "spool"),
            "FASTPROOF_ADMIN_TOKENS_FILE": str(tokens),
            "FASTPROOF_LOG_FORMAT": "text",
        },
        stdout=log,
        stderr=subprocess.STDOUT,
    )
    try:
        deadline = time.time() + 60
        while True:
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{ops}/readyz", timeout=2) as r:
                    if r.status == 200:
                        break
            except (OSError, urllib.error.URLError):
                pass
            if proc.poll() is not None or time.time() > deadline:
                raise RuntimeError(f"the server did not start; see {work / 'server.log'}")
            time.sleep(0.2)
        yield Server(f"http://127.0.0.1:{port}", TOKEN, work)
    finally:
        proc.terminate()
        try:
            proc.wait(15)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        log.close()
        try:
            pgmini.execute(db_url, f"drop database if exists {name} with (force)")
        except Exception:
            pass


def wait_for(fn, timeout: float = 30.0):
    deadline = time.time() + timeout
    while True:
        v = fn()
        if v or time.time() > deadline:
            return v
        time.sleep(0.3)


def single_launch(server: Server, key: str):
    items = server.launches(key)
    assert len(items) == 1, [i["externalId"] for i in items]
    return items[0]


def by_name(server: Server, key: str, launch_id: str):
    return {r["name"]: r for r in server.results(key, launch_id)}
