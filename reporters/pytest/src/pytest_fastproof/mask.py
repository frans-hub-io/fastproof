"""Secret masking at the source: values of matching environment variables, explicit values and patterns."""

from __future__ import annotations

import fnmatch
import os
import re
from typing import Iterable, Mapping, Optional, Pattern, Sequence, Union
from urllib.parse import quote

MASK = "****"

DEFAULT_SECRET_ENV = (
    "FASTPROOF_TOKEN",
    "*TOKEN*",
    "*SECRET*",
    "*PASSWORD*",
    "*PASSWD*",
    "*_KEY",
    "*APIKEY*",
    "*API_KEY*",
    "*CREDENTIAL*",
)

MIN_SECRET_LENGTH = 4
_NOT_SECRET = {"true", "false", "yes", "no", "on", "off", "null", "none", "undefined"}


class Masker:
    """Replaces secrets with ``****``. Shorter or boolean-looking values are ignored."""

    def __init__(
        self,
        env_names: Optional[Sequence[str]] = None,
        values: Iterable[str] = (),
        patterns: Iterable[Union[str, Pattern[str]]] = (),
        env: Optional[Mapping[str, str]] = None,
    ) -> None:
        env = os.environ if env is None else env
        globs = [g.upper() for g in (DEFAULT_SECRET_ENV if env_names is None else env_names)]
        found = set()

        def add(v: Optional[str]) -> None:
            if not v:
                return
            t = v.strip()
            if len(t) < MIN_SECRET_LENGTH or t.lower() in _NOT_SECRET or re.fullmatch(r"\d{1,6}", t):
                return
            found.add(t)
            enc = quote(t, safe="")
            if enc != t:
                found.add(enc)

        for k, v in env.items():
            if any(fnmatch.fnmatchcase(k.upper(), g) for g in globs):
                add(v)
        for v in values:
            add(v)
        ordered = sorted(found, key=len, reverse=True)
        self._literal = re.compile("|".join(re.escape(v) for v in ordered)) if ordered else None
        self._patterns = [p if isinstance(p, re.Pattern) else re.compile(p) for p in patterns]

    @property
    def active(self) -> bool:
        return self._literal is not None or bool(self._patterns)

    def mask(self, s):
        if not s or not isinstance(s, str):
            return s
        out = s
        if self._literal is not None:
            out = self._literal.sub(MASK, out)
        for p in self._patterns:
            out = p.sub(MASK, out)
        return out
