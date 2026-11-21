import json
import logging
import os
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.request import Request, urlopen

import pytest

import pytest_fastproof as fastproof

log = logging.getLogger("sample")
SECRET = os.environ.get("SAMPLE_API_TOKEN", "no-secret")


def _state(name):
    d = os.environ.get("SAMPLE_STATE_DIR", ".")
    return os.path.join(d, name)


@pytest.mark.smoke
def test_passes_with_logs_steps_and_attachments():
    log.info("starting with token %s", SECRET)
    with fastproof.step("outer"):
        with fastproof.step("inner"):
            print(f"stdout line with {SECRET}")
            log.warning("inner warning")
    fastproof.attach(f"body with {SECRET}", name=f"note-{SECRET}.txt")
    fastproof.attach(b"\x00" * 2048, name="shared.bin")
    assert 1 + 1 == 2


def test_fails_assertion():
    fastproof.attach(b"\x00" * 2048, name="shared.bin")
    assert 1 == 2, "numbers differ"


def test_broken_by_exception():
    raise RuntimeError("backend exploded")


@pytest.mark.flaky(reruns=2)
def test_flaky_passes_on_rerun():
    path = _state("flaky.count")
    n = int(open(path).read()) if os.path.exists(path) else 0
    with open(path, "w") as f:
        f.write(str(n + 1))
    log.info("flaky attempt %s", n)
    assert n >= 1, f"attempt {n} fails"


def test_skipped():
    pytest.skip("not today")


@pytest.mark.xfail(reason="known bug")
def test_expected_failure():
    assert False


@pytest.mark.parametrize("user,password", [("alice", SECRET), ("bob", "plain-password-value")])
def test_parametrized(user, password):
    assert user


def test_traceparent_reaches_backend(fastproof_trace):
    seen = {}

    class H(BaseHTTPRequestHandler):
        def do_GET(self):
            seen["traceparent"] = self.headers.get("traceparent")
            seen["baggage"] = self.headers.get("baggage")
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"ok")

        def log_message(self, *args):
            pass

    srv = HTTPServer(("127.0.0.1", 0), H)
    t = threading.Thread(target=srv.serve_forever, daemon=True)
    t.start()
    try:
        url = f"http://127.0.0.1:{srv.server_address[1]}/api"
        with urlopen(Request(url, headers=fastproof_trace.headers()), timeout=10) as r:
            assert r.read() == b"ok"
    finally:
        srv.shutdown()
    assert seen["traceparent"] == fastproof_trace.traceparent
    assert fastproof.trace() == fastproof_trace
    fastproof.attach(json.dumps(seen), name="seen-headers.json", content_type="application/json")
