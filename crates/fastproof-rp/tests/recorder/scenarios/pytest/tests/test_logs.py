import logging
import os
import pytest

LOG = logging.getLogger("rec")
HERE = os.path.dirname(__file__)


def test_levels(rp_logger):
    rp_logger.debug("debug line")
    rp_logger.info("info line")
    rp_logger.warning("warning line")
    rp_logger.error("error line")
    rp_logger.critical("critical line")
    print("stdout line")


def test_attachment(rp_logger):
    rp_logger.info("with text file", attachment={"name": "note.txt", "data": b"hello attachment\n", "mime": "text/plain"})
    png = bytes.fromhex("89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d49444154789c6360f8cf00000301010018dd8db00000000049454e44ae426082")
    rp_logger.info("with image", attachment={"name": "pixel.png", "data": png, "mime": "image/png"})
    rp_logger.error("with binary", attachment={"name": "blob.bin", "data": bytes(range(256)), "mime": "application/octet-stream"})


def test_attachment_then_fail(rp_logger):
    rp_logger.info("before fail", attachment={"name": "log.txt", "data": b"x" * 2048, "mime": "text/plain"})
    raise AssertionError("failed after attachment")


def test_multiline_trace():
    def inner():
        raise RuntimeError("deep\nsecond line")
    inner()
