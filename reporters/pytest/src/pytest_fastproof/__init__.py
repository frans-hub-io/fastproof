"""pytest-fastproof: report pytest runs to FastProof live.

In tests::

    import pytest_fastproof as fastproof

    fastproof.attach(b"...", name="response.json", content_type="application/json")
    fastproof.attach_file("screenshot.png")
    with fastproof.step("pay"):
        ...
    headers = fastproof.trace().headers()
"""

from __future__ import annotations

import mimetypes
import os
from contextlib import contextmanager
from typing import Iterator, Optional, Union

from .api import VERSION as __version__
from .trace import TraceContext

__all__ = ["attach", "attach_file", "step", "trace", "TraceContext", "__version__"]


def _executor():
    from .plugin import current_executor

    return current_executor()


def attach(body: Union[bytes, str], name: str, content_type: Optional[str] = None) -> None:
    """Attaches content to the current attempt; uploaded after the attempt (deduplicated by sha256)."""
    ex = _executor()
    if ex is None:
        return
    data = body.encode("utf-8") if isinstance(body, str) else bytes(body)
    ct = content_type or mimetypes.guess_type(name)[0] or ("text/plain" if isinstance(body, str) else "application/octet-stream")
    ex.attach(name, ct, body=data)


def attach_file(path: Union[str, "os.PathLike[str]"], name: Optional[str] = None, content_type: Optional[str] = None) -> None:
    """Attaches a file to the current attempt; it is streamed from disk after the attempt."""
    ex = _executor()
    if ex is None:
        return
    p = os.fspath(path)
    n = name or os.path.basename(p)
    ct = content_type or mimetypes.guess_type(p)[0] or "application/octet-stream"
    ex.attach(n, ct, path=p)


@contextmanager
def step(name: str) -> Iterator[None]:
    """A named step: streamed live and kept in the result's step tree."""
    ex = _executor()
    if ex is None:
        yield
        return
    with ex.step(name):
        yield


def trace() -> Optional[TraceContext]:
    """Trace context of the current attempt, or ``None`` outside a test or with reporting off."""
    ex = _executor()
    att = ex.current if ex is not None else None
    return att.trace if att is not None else None
