"""W3C trace context per attempt (``traceparent`` and ``baggage``) for FastMock and traced backends."""

from __future__ import annotations

import os
import re
import secrets
from dataclasses import dataclass, field
from typing import Dict, Mapping, Optional
from urllib.parse import quote

_TRACEPARENT = re.compile(r"^00-([0-9a-f]{32})-([0-9a-f]{16})-[0-9a-f]{2}$")


def _non_zero(h: str) -> str:
    return h[:-1] + "1" if set(h) == {"0"} else h


def random_span_id() -> str:
    return _non_zero(secrets.token_hex(8))


def format_traceparent(trace_id: str, span_id: str) -> str:
    return f"00-{trace_id}-{span_id}-01"


def parse_traceparent(v: Optional[str]):
    m = _TRACEPARENT.match((v or "").strip())
    if not m or set(m.group(1)) == {"0"} or set(m.group(2)) == {"0"}:
        return None
    return m.group(1), m.group(2)


def format_baggage(launch: str, test: str, attempt: int) -> str:
    def enc(v: str) -> str:
        return quote(v, safe="")[:256]

    return f"fastproof.launch={enc(launch)},fastproof.test={enc(test)},fastproof.attempt={attempt}"


@dataclass(frozen=True)
class TraceContext:
    """Trace context of one attempt; ``headers()`` for HTTP clients, ``env()`` for child processes."""

    trace_id: str
    span_id: str
    baggage: str
    traceparent: str = field(init=False)

    def __post_init__(self) -> None:
        object.__setattr__(self, "traceparent", format_traceparent(self.trace_id, self.span_id))

    @classmethod
    def new(cls, launch: str, test: str, attempt: int) -> "TraceContext":
        return cls(_non_zero(secrets.token_hex(16)), random_span_id(), format_baggage(launch, test, attempt))

    def headers(self, new_span: bool = False) -> Dict[str, str]:
        span = random_span_id() if new_span else self.span_id
        return {"traceparent": format_traceparent(self.trace_id, span), "baggage": self.baggage}

    def env(self, base: Optional[Mapping[str, str]] = None) -> Dict[str, str]:
        out = dict(os.environ if base is None else base)
        out["TRACEPARENT"] = self.traceparent
        out["BAGGAGE"] = self.baggage
        return out
