import json
import os
import struct
import zlib

import allure
import pytest


def tiny_png():
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    raw = b"".join(b"\x00" + bytes([255, 0, 0]) * 4 for _ in range(4))
    header = struct.pack(">IIBBBBB", 4, 4, 8, 2, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


@pytest.fixture(scope="module")
def api_client():
    with allure.step("Open API session"):
        allure.attach("base_url=https://api.example.com", name="session", attachment_type=allure.attachment_type.TEXT)
    client = {"base_url": "https://api.example.com", "token": "redacted"}
    yield client
    with allure.step("Close API session"):
        allure.attach(json.dumps({"closed": True}), name="close", attachment_type=allure.attachment_type.JSON)


@pytest.fixture
def cart():
    items = [{"sku": "A-1", "qty": 2, "price": 10}]
    yield items
    items.clear()


@pytest.fixture
def database():
    with allure.step("Connect to database at 198.51.100.10"):
        raise RuntimeError("database unavailable: connection refused (198.51.100.10:5432)")
    yield None


@pytest.fixture
def attempt_counter():
    path = os.environ["FASTPROOF_COUNTER_FILE"]

    def bump(name):
        counts = {}
        if os.path.exists(path):
            with open(path) as fh:
                counts = json.load(fh)
        counts[name] = counts.get(name, 0) + 1
        with open(path, "w") as fh:
            json.dump(counts, fh)
        return counts[name]

    return bump
