import logging
import pytest
from reportportal_client import RPLogger


@pytest.fixture(scope="session")
def rp_logger():
    logging.setLoggerClass(RPLogger)
    logger = logging.getLogger("rp-recording")
    logger.setLevel(logging.DEBUG)
    return logger


@pytest.fixture
def good_fixture():
    yield 1


@pytest.fixture
def broken_setup():
    raise RuntimeError("setup exploded")


@pytest.fixture
def broken_teardown():
    yield 1
    raise RuntimeError("teardown exploded")
