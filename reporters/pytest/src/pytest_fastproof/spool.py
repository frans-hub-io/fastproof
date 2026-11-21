"""The offline spool (format in reporters/PROTOCOL.md), shared with the Playwright reporter:
``<root>/<session>/meta.json``, ``results.ndjson``, ``progress.json``, ``blobs/<sha256>``."""

from __future__ import annotations

import json
import os
import shutil
import socket
import time
from typing import Any, Dict, List, Optional

from .api import Api, ApiError

SPOOL_VERSION = 1
BLOB_PREFIX = "blob:"


def dir_size(path: str) -> int:
    total = 0
    for root, _dirs, files in os.walk(path):
        for f in files:
            try:
                total += os.path.getsize(os.path.join(root, f))
            except OSError:
                continue
    return total


def _write_atomic(path: str, data: str) -> None:
    tmp = f"{path}.{os.getpid()}.tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(data)
    os.replace(tmp, path)


class SpoolSession:
    """One reporter process's session; all writes respect a byte budget shared by the spool root."""

    def __init__(self, root: str, session_id: str, meta: Dict[str, Any], max_bytes: int) -> None:
        self.root = root
        self.id = session_id
        self.dir = os.path.join(root, session_id)
        self.meta = meta
        self.max_bytes = max_bytes
        self.used = dir_size(root) if os.path.isdir(root) else 0
        self.opened = False
        self.dropped = {"results": 0, "blobs": 0}
        self.written = {"results": 0, "blobs": 0}

    def _open(self) -> bool:
        if self.opened:
            return True
        try:
            os.makedirs(os.path.join(self.dir, "blobs"), exist_ok=True)
            _write_atomic(os.path.join(self.dir, "meta.json"), json.dumps(self.meta))
            self.opened = True
        except OSError:
            return False
        return True

    def add_blob(self, sha256: str, size: int, low: bool, path: Optional[str] = None, body: Optional[bytes] = None) -> bool:
        """Copies an attachment in. ``low`` priority (passed tests) may use only half of the budget."""
        if not self._open():
            return False
        target = os.path.join(self.dir, "blobs", sha256)
        if os.path.exists(target):
            return True
        limit = self.max_bytes / 2 if low else self.max_bytes
        if self.used + size > limit:
            self.dropped["blobs"] += 1
            return False
        tmp = f"{target}.{os.getpid()}.tmp"
        try:
            if path is not None:
                shutil.copyfile(path, tmp)
            else:
                with open(tmp, "wb") as f:
                    f.write(body or b"")
            os.replace(tmp, target)
        except OSError:
            self.dropped["blobs"] += 1
            return False
        self.used += size
        self.written["blobs"] += 1
        return True

    def append(self, entry: Dict[str, Any]) -> bool:
        if not self._open():
            self.dropped["results"] += 1
            return False
        line = (json.dumps(entry, ensure_ascii=False) + "\n").encode("utf-8")
        if self.used + len(line) > self.max_bytes:
            self.dropped["results"] += 1
            return False
        try:
            fd = os.open(os.path.join(self.dir, "results.ndjson"), os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
            try:
                os.write(fd, line)
                os.fsync(fd)
            finally:
                os.close(fd)
        except OSError:
            self.dropped["results"] += 1
            return False
        self.used += len(line)
        self.written["results"] += 1
        return True

    def end(self, finish: Optional[str]) -> None:
        if not self.opened:
            return
        self.meta["ended"] = True
        if finish:
            self.meta["finish"] = finish
        try:
            _write_atomic(os.path.join(self.dir, "meta.json"), json.dumps(self.meta))
        except OSError:
            pass


def _pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except PermissionError:
        return True
    except OSError:
        return False


def list_sessions(root: str, exclude: Optional[str] = None) -> List[Dict[str, Any]]:
    """Sessions that can be sent now: ended, or left by a process that is gone."""
    try:
        names = sorted(os.listdir(root))
    except OSError:
        return []
    out = []
    for name in names:
        if name == exclude or name.startswith("."):
            continue
        d = os.path.join(root, name)
        try:
            with open(os.path.join(d, "meta.json"), encoding="utf-8") as f:
                meta = json.load(f)
        except (OSError, ValueError):
            continue
        if meta.get("v") != SPOOL_VERSION:
            continue
        running = (
            not meta.get("ended")
            and meta.get("host") == socket.gethostname()
            and meta.get("pid") != os.getpid()
            and _pid_alive(int(meta.get("pid") or 0))
        )
        if not running:
            out.append({"id": name, "dir": d, "meta": meta})
    return out


def _replace_blob_ids(node: Dict[str, Any], ids: Dict[str, str]) -> None:
    atts = node.get("attachments")
    if atts:
        kept = []
        for a in atts:
            aid = a.get("id", "")
            if aid.startswith(BLOB_PREFIX):
                real = ids.get(aid[len(BLOB_PREFIX) :])
                if not real:
                    continue
                a["id"] = real
            kept.append(a)
        node["attachments"] = kept
    for s in node.get("steps") or []:
        _replace_blob_ids(s, ids)


def flush_spool(
    api: Api, root: str, exclude: Optional[str] = None, deadline: Optional[float] = None,
    project: Optional[str] = None, batch: int = 200,
) -> Dict[str, Any]:
    """Sends spooled sessions: resolves each launch by external id, uploads blobs once per hash, posts results
    (idempotent by client id), finishes the launch when asked, deletes the session. Resumable."""
    out: Dict[str, Any] = {"sessions": 0, "results": 0, "failed": 0, "remaining": 0, "errors": []}

    def late() -> bool:
        return deadline is not None and time.time() > deadline

    for s in list_sessions(root, exclude):
        meta = s["meta"]
        if project and meta.get("project") != project:
            continue
        try:
            with open(os.path.join(s["dir"], "results.ndjson"), encoding="utf-8") as f:
                lines = [ln for ln in f.read().split("\n") if ln.strip()]
        except OSError:
            lines = []
        try:
            with open(os.path.join(s["dir"], "progress.json"), encoding="utf-8") as f:
                done = int(json.load(f).get("lines", 0))
        except (OSError, ValueError):
            done = 0
        if late():
            out["remaining"] += len(lines) - done
            continue
        try:
            launch = api.start_launch(meta["launch"], retries=2)
            blob_ids: Dict[str, str] = {}
            while done < len(lines) and not late():
                chunk = lines[done : done + batch]
                results = []
                for ln in chunk:
                    try:
                        e = json.loads(ln)
                    except ValueError:
                        out["failed"] += 1
                        continue
                    for b in e.get("blobs") or []:
                        if b["sha256"] in blob_ids:
                            continue
                        p = os.path.join(s["dir"], "blobs", b["sha256"])
                        if not os.path.exists(p):
                            continue
                        r = api.upload_attachment(launch["id"], b["name"], b["contentType"], path=p, retries=2)
                        blob_ids[b["sha256"]] = r["id"]
                    _replace_blob_ids(e["result"], blob_ids)
                    results.append(e["result"])
                if results:
                    summary = api.post_results(launch["id"], results, retries=2)
                    out["results"] += summary["accepted"] + summary["duplicates"]
                    out["failed"] += len(summary["rejected"])
                    out["errors"].extend(r.get("message", "") for r in summary["rejected"][:3])
                done += len(chunk)
                _write_atomic(os.path.join(s["dir"], "progress.json"), json.dumps({"lines": done}))
            if done >= len(lines):
                if meta.get("ended") and meta.get("finish"):
                    api.finish_launch(launch["id"], meta["finish"], retries=2)
                if meta.get("ended") or meta.get("pid") != os.getpid():
                    shutil.rmtree(s["dir"], ignore_errors=True)
                out["sessions"] += 1
            else:
                out["remaining"] += len(lines) - done
        except ApiError as e:
            out["remaining"] += len(lines) - done
            out["errors"].append(str(e))
            if not e.fatal:
                break
        except (OSError, KeyError, ValueError) as e:
            out["remaining"] += len(lines) - done
            out["errors"].append(str(e))
    return out
