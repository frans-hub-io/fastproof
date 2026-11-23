import json
import logging
import os
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.request import Request, urlopen

import allure
import pytest

import fastproof_allure_pytest as fastproof

log = logging.getLogger("sample")
SECRET = os.environ.get("SAMPLE_API_TOKEN", "no-secret")


def _state(name):
    return os.path.join(os.environ.get("SAMPLE_STATE_DIR", "."), name)


@allure.feature("checkout")
def test_passes_with_steps_and_attachments():
    log.info("starting with token %s", SECRET)
    with allure.step("outer"):
        with allure.step("inner"):
            print(f"stdout line with {SECRET}")
    allure.attach(f"body with {SECRET}", name=f"note-{SECRET}.txt", attachment_type=allure.attachment_type.TEXT)
    allure.attach(b"\x00" * 2048, name="shared.bin", attachment_type=allure.attachment_type.PNG)
    allure.dynamic.parameter("api_token", SECRET, mode=allure.parameter_mode.MASKED)


def test_fails_assertion():
    allure.attach(b"\x00" * 2048, name="shared.bin", attachment_type=allure.attachment_type.PNG)
    assert 1 == 2, "numbers differ"


def test_broken_by_exception():
    raise RuntimeError("backend exploded")


@pytest.mark.flaky(reruns=2)
def test_flaky_passes_on_rerun():
    path = _state("flaky.count")
    n = int(open(path).read()) if os.path.exists(path) else 0
    open(path, "w").write(str(n + 1))
    assert n >= 1


@allure.label("as_id", "SHOP-42")
def test_with_explicit_id():
    pass


@pytest.mark.parametrize("user", ["alice", "bob"])
def test_parametrized(user):
    assert user


def test_skipped():
    pytest.skip("not today")


def test_traceparent_reaches_backend():
    seen = {}

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            seen.update({k.lower(): v for k, v in self.headers.items()})
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"ok")

        def log_message(self, *args):
            pass

    server = HTTPServer(("127.0.0.1", 0), Handler)
    t = threading.Thread(target=server.serve_forever, daemon=True)
    t.start()
    try:
        urlopen(Request(f"http://127.0.0.1:{server.server_address[1]}/", headers=fastproof.trace().headers())).read()
    finally:
        server.shutdown()
    allure.attach(json.dumps(seen), name="seen-headers.json", attachment_type=allure.attachment_type.JSON)
    assert seen["traceparent"] == fastproof.trace().traceparent
