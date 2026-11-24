"""The FastProof HTTP API over ``urllib`` (no third-party dependencies)."""

from __future__ import annotations

import email.utils
import json
import os
import random
import time
import urllib.error
import urllib.request
from typing import Any, Dict, List, Optional, Sequence
from urllib.parse import quote

VERSION = "0.1.0"
USER_AGENT = f"pytest-fastproof/{VERSION}"


class ApiError(Exception):
    """A failed call; ``fatal`` means retrying the same request cannot help (4xx other than 408/429)."""

    def __init__(self, message: str, status: int, fatal: bool) -> None:
        super().__init__(message)
        self.status = status
        self.fatal = fatal


class _Response:
    def __init__(self, status: int, body: Any, retry_after: Optional[float] = None, error: str = "") -> None:
        self.status = status
        self.body = body
        self.retry_after = retry_after
        self.error = error

    @property
    def ok(self) -> bool:
        return 200 <= self.status < 300

    @property
    def retryable(self) -> bool:
        return self.status == 0 or self.status in (408, 429) or self.status >= 500

    def describe(self) -> str:
        if self.status == 0:
            return f"server unreachable ({self.error or 'network error'})"
        if self.status < 0:
            return self.error or "local error"
        msg = self.body.get("message") if isinstance(self.body, dict) else None
        return f"HTTP {self.status}" + (f": {msg}" if msg else "")


def _retry_after(v: Optional[str]) -> Optional[float]:
    if not v:
        return None
    try:
        return min(max(float(v), 0.0), 60.0)
    except ValueError:
        pass
    try:
        d = email.utils.parsedate_to_datetime(v)
        return min(max(d.timestamp() - time.time(), 0.0), 60.0)
    except (TypeError, ValueError):
        return None


def backoff(attempt: int, base: float = 0.25, cap: float = 8.0) -> float:
    exp = min(cap, base * (2 ** min(attempt, 16)))
    return exp / 2 + random.random() * exp / 2


def encode_header(v: str) -> str:
    s = v.replace("\r", " ").replace("\n", " ")[:255]
    return s if all(0x20 <= ord(c) <= 0x7E for c in s) else quote(s, safe="")


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


class Api:
    """Typed calls with bounded retries. Methods raise :class:`ApiError`; callers decide what to do."""

    def __init__(self, url: str, token: str, project: str, timeout: float = 15.0, retries: int = 4) -> None:
        self.base = url.rstrip("/")
        self.token = token
        self.project = project
        self.timeout = timeout
        self.retries = retries
        self._opener = urllib.request.build_opener(_NoRedirect)

    def _path(self, launch_id: Optional[str] = None) -> str:
        p = f"/api/v1/projects/{quote(self.project, safe='')}"
        return f"{p}/launches/{quote(launch_id, safe='')}" if launch_id else p

    def request(
        self,
        method: str,
        path: str,
        data: Any = None,
        content_type: Optional[str] = None,
        headers: Optional[Dict[str, str]] = None,
        timeout: Optional[float] = None,
        file: Optional[str] = None,
        fileobj: Any = None,
    ) -> _Response:
        h = {"Accept": "application/json", "User-Agent": USER_AGENT}
        if self.token:
            h["Authorization"] = f"Bearer {self.token}"
        h.update(headers or {})
        body: Any = data
        fh = None
        if fileobj is not None:
            try:
                size = os.fstat(fileobj.fileno()).st_size
                fileobj.seek(0)
            except OSError as e:
                return _Response(-1, None, error=f"cannot read the attachment: {e}")
            body = fileobj
            h["Content-Length"] = str(size)
        elif file is not None:
            try:
                size = os.path.getsize(file)
                fh = open(file, "rb")
            except OSError as e:
                return _Response(-1, None, error=f"cannot read {file}: {e}")
            body = fh
            h["Content-Length"] = str(size)
        if content_type:
            h["Content-Type"] = content_type
        req = urllib.request.Request(self.base + path, data=body, headers=h, method=method)
        try:
            with self._opener.open(req, timeout=timeout or self.timeout) as res:
                raw = res.read()
                return _Response(res.status, _parse(raw))
        except urllib.error.HTTPError as e:
            try:
                raw = e.read()
            except OSError:
                raw = b""
            return _Response(e.code, _parse(raw), _retry_after(e.headers.get("Retry-After") if e.headers else None))
        except (urllib.error.URLError, OSError, ValueError) as e:
            reason = getattr(e, "reason", None) or e
            return _Response(0, None, error=str(reason))
        finally:
            if fh is not None:
                fh.close()

    def call(self, fn, retries: Optional[int] = None) -> _Response:
        retries = self.retries if retries is None else retries
        last = _Response(0, None)
        for i in range(retries + 1):
            last = fn()
            if last.ok:
                return last
            if not last.retryable:
                raise ApiError(last.describe(), last.status, True)
            if i < retries:
                time.sleep(last.retry_after if last.retry_after is not None else backoff(i))
        raise ApiError(last.describe(), last.status, False)

    def start_launch(self, start: Dict[str, Any], retries: Optional[int] = None) -> Dict[str, Any]:
        body = json.dumps(start).encode()
        r = self.call(lambda: self.request("POST", f"{self._path()}/launches", body, "application/json"), retries)
        if not isinstance(r.body, dict) or not isinstance(r.body.get("id"), str):
            raise ApiError("unexpected launch response", r.status, True)
        return r.body

    def post_results(self, launch_id: str, results: Sequence[Dict[str, Any]], retries: Optional[int] = None) -> Dict[str, Any]:
        body = _ndjson(results)
        r = self.call(
            lambda: self.request("POST", f"{self._path(launch_id)}/results", body, "application/x-ndjson"), retries
        )
        b = r.body if isinstance(r.body, dict) else {}
        return {"accepted": b.get("accepted", 0), "duplicates": b.get("duplicates", 0), "rejected": b.get("rejected") or []}

    def post_logs(self, launch_id: str, lines: Sequence[Dict[str, Any]], retries: Optional[int] = None) -> str:
        """``"ok"``, or ``"unsupported"`` when the server has no logs endpoint (404/405/501)."""
        body = _ndjson(lines)
        try:
            self.call(lambda: self.request("POST", f"{self._path(launch_id)}/logs", body, "application/x-ndjson"), retries)
            return "ok"
        except ApiError as e:
            if e.status in (404, 405, 501):
                return "unsupported"
            raise

    def upload_attachment(
        self, launch_id: str, name: str, content_type: str, path: Optional[str] = None, body: Optional[bytes] = None,
        retries: Optional[int] = None, fileobj: Any = None,
    ) -> Dict[str, Any]:
        headers = {"X-FastProof-Attachment-Name": encode_header(name)}
        r = self.call(
            lambda: self.request(
                "POST", f"{self._path(launch_id)}/attachments", body if path is None and fileobj is None else None, content_type,
                headers, timeout=120.0, file=path, fileobj=fileobj,
            ),
            retries,
        )
        if not isinstance(r.body, dict) or not isinstance(r.body.get("id"), str):
            raise ApiError("unexpected attachment response", r.status, True)
        return r.body

    def direct_upload(
        self, launch_id: str, sha256: str, size: int, name: str, content_type: str, retries: Optional[int] = None
    ) -> Dict[str, Any]:
        """Asks for a pre-signed URL for one attachment; raises ``ApiError`` 404/405/501 when the server has none."""
        body = json.dumps(
            {"items": [{"sha256": sha256, "size": size, "name": name, "contentType": content_type}]}
        ).encode()
        r = self.call(
            lambda: self.request("POST", f"{self._path(launch_id)}/attachments/direct", body, "application/json"),
            retries,
        )
        items = r.body.get("items") if isinstance(r.body, dict) else None
        if not items or not isinstance(items[0], dict) or not isinstance(items[0].get("status"), str):
            raise ApiError("unexpected direct upload response", r.status, True)
        return items[0]

    def complete_direct(self, launch_id: str, upload_id: str, retries: Optional[int] = None) -> str:
        """The attachment id; retries ``409`` (not visible yet, being verified) a few times."""
        path = f"{self._path(launch_id)}/attachments/direct/{quote(upload_id, safe='')}/complete"
        last: Optional[ApiError] = None
        for i in range(4):
            try:
                r = self.call(lambda: self.request("POST", path, timeout=120.0), retries)
            except ApiError as e:
                if e.status != 409:
                    raise
                last = e
                time.sleep(backoff(i, 0.5))
                continue
            if not isinstance(r.body, dict) or not isinstance(r.body.get("id"), str):
                raise ApiError("unexpected completion response", r.status, True)
            return r.body["id"]
        raise last  # type: ignore[misc]

    def finish_launch(self, launch_id: str, status: str, retries: Optional[int] = None) -> None:
        body = json.dumps({"status": status}).encode()
        self.call(lambda: self.request("POST", f"{self._path(launch_id)}/finish", body, "application/json"), retries)


def put_presigned(
    url: str, headers: Dict[str, str], size: int, path: Optional[str] = None, body: Optional[bytes] = None,
    timeout: float = 120.0,
) -> int:
    """PUTs a file or bytes to a pre-signed URL with exactly ``headers`` and no FastProof token. Returns the HTTP
    status, or 0 when the store did not answer (not reachable from here)."""
    h = dict(headers)
    h["Content-Length"] = str(size)
    fh = None
    try:
        data: Any = body
        if path is not None:
            fh = open(path, "rb")
            data = fh
        req = urllib.request.Request(url, data=data, headers=h, method="PUT")
        opener = urllib.request.build_opener(_NoRedirect)
        with opener.open(req, timeout=timeout) as res:
            res.read()
            return res.status
    except urllib.error.HTTPError as e:
        return e.code
    except (urllib.error.URLError, OSError, ValueError):
        return 0
    finally:
        if fh is not None:
            fh.close()


def _ndjson(items: Sequence[Dict[str, Any]]) -> bytes:
    return ("\n".join(json.dumps(i, ensure_ascii=False) for i in items) + "\n").encode("utf-8")


def _parse(raw: bytes) -> Any:
    if not raw:
        return None
    try:
        return json.loads(raw)
    except ValueError:
        return raw.decode("utf-8", "replace")


__all__: List[str] = ["Api", "ApiError", "VERSION", "USER_AGENT", "backoff", "encode_header"]
