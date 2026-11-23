"""fastproof-allure-pytest: live FastProof reporting for projects that use allure-pytest.

Install it next to ``allure-pytest`` and ``pytest-fastproof`` and run pytest with ``--alluredir`` as before;
the Allure results are written as usual and every attempt is also sent to FastProof while it runs::

    import fastproof_allure_pytest as fastproof

    headers = fastproof.trace().headers()
"""

from __future__ import annotations

from typing import Optional

from pytest_fastproof.api import VERSION as __version__
from pytest_fastproof.trace import TraceContext

__all__ = ["trace", "TraceContext", "__version__"]


def trace() -> Optional[TraceContext]:
    """Trace context of the current attempt, or ``None`` outside a test or with reporting off."""
    from .plugin import current_trace

    return current_trace()
