import itertools
import pytest

COUNTER = itertools.count()


def test_pass(good_fixture):
    assert good_fixture == 1


def test_fail():
    assert 1 == 2, "values differ"


@pytest.mark.skip(reason="not today")
def test_skipped():
    pass


@pytest.mark.xfail(reason="known")
def test_xfail():
    assert False


@pytest.mark.parametrize("a,b", [(1, 2), (3, 4), (5, 5)])
def test_param(a, b):
    assert a + b > 0
    if a == b:
        raise ValueError("equal")


@pytest.mark.issue(reason="tracked defect", issue_id="PROJ-123", issue_type="PB")
def test_known_defect():
    assert False, "defect"


@pytest.mark.flaky(reruns=2)
def test_flaky():
    assert next(COUNTER) >= 2


@pytest.mark.tag_one
@pytest.mark.tag_two
def test_marked():
    pass


def test_setup_error(broken_setup):
    pass


def test_teardown_error(broken_teardown):
    pass


class TestNested:
    def test_in_class_ok(self):
        pass

    def test_in_class_fail(self):
        raise KeyError("missing")

    class TestInner:
        def test_inner(self):
            pass
