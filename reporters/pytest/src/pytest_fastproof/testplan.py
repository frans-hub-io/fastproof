"""Allure-format test plans (``{"version": "1.0", "tests": [{"id", "selector"}]}``)."""

from __future__ import annotations

import json
import os
from typing import Any, Dict, Iterable, List, Mapping, Optional, Set


def load_test_plan(path: Optional[str] = None, env: Optional[Mapping[str, str]] = None) -> Optional[Dict[str, Any]]:
    """Reads the plan from ``path``, ``FASTPROOF_TESTPLAN`` or ``ALLURE_TESTPLAN_PATH``; ``None`` when unset."""
    env = os.environ if env is None else env
    p = path or env.get("FASTPROOF_TESTPLAN") or env.get("ALLURE_TESTPLAN_PATH")
    if not p:
        return None
    with open(p, encoding="utf-8") as f:
        plan = json.load(f)
    if not isinstance(plan, dict) or not isinstance(plan.get("tests"), list):
        raise ValueError(f"{p}: not a test plan (expected an object with a tests array)")
    return plan


def strip_params(nodeid: str) -> str:
    """``tests/test_a.py::test_x[1-2]`` -> ``tests/test_a.py::test_x``."""
    if nodeid.endswith("]") and "[" in nodeid:
        return nodeid[: nodeid.index("[", nodeid.rfind("::") if "::" in nodeid else 0)]
    return nodeid


def allure_full_name(nodeid: str) -> str:
    """allure-pytest's full name: ``tests.test_a.TestX#test_y``."""
    parts = strip_params(nodeid).split("::")
    path = parts[0]
    if path.endswith(".py"):
        path = path[:-3]
    package = path.replace("/", ".").replace("\\", ".")
    classes = parts[1:-1]
    name = parts[-1] if len(parts) > 1 else ""
    return ".".join([package] + classes) + "#" + name


def candidates(nodeid: str) -> Set[str]:
    base = strip_params(nodeid)
    return {nodeid, base, allure_full_name(nodeid)}


class TestPlan:
    __test__ = False

    def __init__(self, plan: Mapping[str, Any]) -> None:
        tests: Iterable[Mapping[str, Any]] = plan.get("tests") or []
        self.ids: Set[str] = set()
        self.selectors: Set[str] = set()
        for t in tests:
            if not isinstance(t, Mapping):
                continue
            if t.get("id") is not None:
                self.ids.add(str(t["id"]))
            if isinstance(t.get("selector"), str) and t["selector"].strip():
                self.selectors.add(t["selector"].strip())

    def __len__(self) -> int:
        return len(self.ids) + len(self.selectors)

    def matches(self, nodeid: str, ids: List[str]) -> bool:
        if any(i in self.ids for i in ids):
            return True
        return bool(candidates(nodeid) & self.selectors)
