"""Conversion of an allure-python ``TestResult`` into a FastProof ``ReportResult`` (reporters/PROTOCOL.md)."""

from __future__ import annotations

from typing import Any, Callable, Dict, List, Optional, Tuple

from pytest_fastproof.client import cut

MAX_STEPS = 2000
MAX_DEPTH = 32
MAX_TEXT = 4 * 1024 * 1024
MAX_LABELS = 200
STATUSES = {"passed", "failed", "broken", "skipped"}

Mask = Callable[[Any], Any]


def value_of(v: Any) -> Any:
    """Enum members of allure-python carry their text in ``value``."""
    return getattr(v, "value", v)


def status_of(v: Any) -> str:
    s = value_of(v)
    return s if s in STATUSES else "unknown"


def ms(v: Any) -> Optional[int]:
    return int(v) if isinstance(v, (int, float)) and v > 0 else None


def parameter(p: Any, mask: Mask) -> Dict[str, Any]:
    mode = value_of(getattr(p, "mode", None))
    raw = getattr(p, "value", None)
    out: Dict[str, Any] = {"name": mask(str(p.name))}
    if mode in ("masked", "hidden"):
        out["mode"] = mode
        out["value"] = "****"
    elif raw is not None:
        out["value"] = mask(cut(str(raw), 1000))
    if getattr(p, "excluded", None):
        out["excluded"] = True
    return out


def error_of(details: Any, mask: Mask) -> Optional[Dict[str, str]]:
    if details is None:
        return None
    message = getattr(details, "message", None)
    trace = getattr(details, "trace", None)
    if not message and not trace:
        return None
    out: Dict[str, str] = {}
    if message:
        out["message"] = mask(cut(str(message), 64 * 1024))
    if trace:
        out["trace"] = mask(cut(str(trace), MAX_TEXT))
    return out


class Walk:
    def __init__(self, mask: Mask, uploads: Dict[str, Dict[str, Any]]) -> None:
        self.mask = mask
        self.uploads = uploads
        self.steps = 0
        self.files: List[Dict[str, Any]] = []
        self.used: Dict[str, bool] = {}

    def attachment(self, a: Any) -> None:
        source = getattr(a, "source", None)
        known = self.uploads.get(source or "")
        if not known or source in self.used:
            return
        self.used[source] = True
        name = self.mask(getattr(a, "name", None) or source)
        ct = getattr(a, "type", None) or known.get("contentType") or "application/octet-stream"
        self.files.append({"name": name, "contentType": ct, "path": known["path"]})

    def step(self, s: Any, depth: int) -> Optional[Dict[str, Any]]:
        if self.steps >= MAX_STEPS or depth > MAX_DEPTH:
            return None
        self.steps += 1
        node: Dict[str, Any] = {"name": self.mask(cut(str(getattr(s, "name", None) or "step"), 1000)), "status": status_of(getattr(s, "status", None))}
        start, stop = ms(getattr(s, "start", None)), ms(getattr(s, "stop", None))
        if start:
            node["start"] = start
        if stop:
            node["stop"] = stop
        if start and stop and stop >= start:
            node["durationMs"] = stop - start
        err = error_of(getattr(s, "statusDetails", None), self.mask)
        if err:
            node["error"] = err
        params = [parameter(p, self.mask) for p in getattr(s, "parameters", None) or []]
        if params:
            node["parameters"] = params
        for a in getattr(s, "attachments", None) or []:
            self.attachment(a)
        kids = []
        for child in getattr(s, "steps", None) or []:
            c = self.step(child, depth + 1)
            if c is not None:
                kids.append(c)
        if kids:
            node["steps"] = kids
        return node


def to_report(
    result: Any,
    rid: str,
    attempt: int,
    traceparent: str,
    mask: Mask,
    uploads: Dict[str, Dict[str, Any]],
    source: Optional[Tuple[str, int]] = None,
) -> Tuple[Dict[str, Any], List[Dict[str, Any]], List[str]]:
    """The final line of one attempt, the files to upload for it and the Allure sources they came from."""
    start, stop = ms(getattr(result, "start", None)), ms(getattr(result, "stop", None))
    name = mask(cut(str(result.name), 1000))
    r: Dict[str, Any] = {
        "id": rid,
        "name": name,
        "status": status_of(result.status),
        "retry": attempt,
        "traceparent": traceparent,
    }
    full = getattr(result, "fullName", None)
    if full:
        r["fullName"] = mask(str(full))
    tcid = getattr(result, "testCaseId", None)
    if tcid:
        r["testCaseId"] = str(tcid)
    desc = getattr(result, "description", None)
    if desc:
        r["description"] = mask(cut(str(desc), 64 * 1024))
    if start:
        r["start"] = start
        r["stop"] = max(stop or start, start)
        r["durationMs"] = r["stop"] - start
    labels = []
    for lb in (getattr(result, "labels", None) or [])[:MAX_LABELS]:
        if lb.name and lb.value is not None:
            labels.append({"name": str(lb.name), "value": mask(cut(str(lb.value), 1000))})
    r["labels"] = labels
    links = []
    for ln in getattr(result, "links", None) or []:
        if ln.url:
            link: Dict[str, Any] = {"url": str(ln.url)}
            if ln.name:
                link["name"] = mask(str(ln.name))
            if value_of(ln.type):
                link["type"] = str(value_of(ln.type))
            links.append(link)
    if links:
        r["links"] = links
    r["parameters"] = [parameter(p, mask) for p in getattr(result, "parameters", None) or []]
    err = error_of(getattr(result, "statusDetails", None), mask)
    if err:
        r["error"] = err
    elif r["status"] in ("failed", "broken") and not err:
        r["error"] = {"message": "the test failed without details"}
    w = Walk(mask, uploads)
    for a in getattr(result, "attachments", None) or []:
        w.attachment(a)
    steps = []
    for s in getattr(result, "steps", None) or []:
        n = w.step(s, 1)
        if n is not None:
            steps.append(n)
    if steps:
        r["steps"] = steps
    if source:
        r["source"] = {"file": source[0], "line": source[1]}
    return r, w.files, list(w.used)
