"""The transport core: a launch, a background sender with batching, live logs with feature detection,
attachment uploads with sha256 dedup, a circuit breaker and the offline spool. Nothing here raises into
the test run; hooks only append to in-memory queues."""

from __future__ import annotations

import hashlib
import json
import os
import secrets
import socket
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from typing import Any, Callable, Dict, List, Optional

from .api import Api, ApiError, VERSION
from .config import Config
from .mask import Masker
from .spool import BLOB_PREFIX, SPOOL_VERSION, SpoolSession, flush_spool

MAX_LOG_MESSAGE = 64 * 1024
MAX_QUEUED_LOGS = 20000
RESULT_BATCH = 500
RESULT_BATCH_BYTES = 4 * 1024 * 1024
BREAKER_FAILURES = 5
MASK_TEXT_ATTACHMENT_BYTES = 4 * 1024 * 1024
FALLBACK_LOGS = "_fallbackLogs"
LOGS_DETECT_WAIT = 2.0


def cut(s: str, limit: int) -> str:
    return s if len(s) <= limit else s[:limit] + "\n…[truncated]"


def is_text(ct: str) -> bool:
    base = ct.split(";")[0].strip().lower()
    return base.startswith("text/") or base in ("application/json", "application/x-ndjson") or base.endswith("+json")


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


class Client:
    def __init__(self, cfg: Config, role: str = "reporter", out: Optional[Callable[[str], None]] = None) -> None:
        self.cfg = cfg
        self.role = role
        self._out = out or (lambda m: sys.stderr.write(m + "\n"))
        self.api = Api(cfg.url, cfg.token, cfg.project, cfg.request_timeout)
        self.masker = Masker(cfg.secret_env, cfg.secret_values, cfg.secret_patterns, cfg.env)
        self.launch_id: Optional[str] = None
        self.launch_number: Optional[int] = None
        self.offline = cfg.offline
        self.offline_reason = "offline mode" if cfg.offline else ""
        self.running = cfg.running_results
        self.logs_state = "unknown" if cfg.live_logs else "off"
        self.failures = 0
        self._lock = threading.Lock()
        self._wake = threading.Condition(self._lock)
        self._queue: List[Dict[str, Any]] = []
        self._logs: List[Dict[str, Any]] = []
        self._seq: Dict[str, int] = {}
        self._inflight: Dict[str, Dict[str, Any]] = {}
        self._uploads: Dict[str, str] = {}
        self._upload_locks: Dict[str, threading.Lock] = {}
        self._warned: set = set()
        self._stop = False
        self._thread: Optional[threading.Thread] = None
        self._pipeline = ThreadPoolExecutor(max_workers=4, thread_name_prefix="fastproof-upload")
        self._send_lock = threading.Lock()
        self.stats = {"sent": 0, "lost": 0, "attachments": 0, "dropped_logs": 0}
        stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S")
        self.session_id = f"{stamp}-{os.getpid()}-{secrets.token_hex(4)}"
        self.spool = SpoolSession(
            cfg.spool_dir,
            self.session_id,
            {
                "v": SPOOL_VERSION,
                "reporter": f"pytest/{VERSION}",
                "project": cfg.project,
                "launch": cfg.launch,
                "createdAt": datetime.now(timezone.utc).isoformat(),
                "pid": os.getpid(),
                "host": socket.gethostname(),
            },
            cfg.spool_max_bytes,
        )
        self._old_spool: Optional[threading.Thread] = None

    def info(self, msg: str) -> None:
        if not self.cfg.quiet:
            self._out(f"[fastproof] {msg}")

    def warn_once(self, key: str, msg: str) -> None:
        if key in self._warned:
            return
        self._warned.add(key)
        self._out(f"[fastproof] {self.masker.mask(msg)}")

    def start(self, flush_old: bool = True) -> None:
        if self.offline:
            return
        self._thread = threading.Thread(target=self._loop, name="fastproof-sender", daemon=True)
        self._thread.start()
        if flush_old:
            self._old_spool = threading.Thread(target=self._flush_old, name="fastproof-spool", daemon=True)
            self._old_spool.start()

    def _flush_old(self) -> None:
        self._ensure_launch(2)
        if self.offline:
            return
        try:
            r = flush_spool(self.api, self.cfg.spool_dir, exclude=self.session_id, project=self.cfg.project)
        except Exception:
            return
        if r["sessions"]:
            self.info(f"sent {r['results']} results left by earlier runs from {self.cfg.spool_dir}")

    def _ensure_launch(self, retries: int = 0) -> None:
        if self.launch_id or self.offline:
            return
        try:
            launch = self.api.start_launch(self.cfg.launch, retries=retries)
        except ApiError as e:
            self._failed(e)
            return
        with self._lock:
            if not self.launch_id:
                self.launch_id = launch["id"]
                self.launch_number = launch.get("number")
            self.failures = 0

    def _failed(self, e: Exception) -> None:
        err = e if isinstance(e, ApiError) else ApiError(str(e), 0, False)
        if err.fatal:
            if err.status in (401, 403, 404):
                self._go_offline(str(err))
            else:
                self.warn_once(f"fatal:{err.status}", f"the server refused a request: {err}")
            return
        self.failures += 1
        if self.failures >= BREAKER_FAILURES:
            self._go_offline(str(err))

    def _go_offline(self, reason: str) -> None:
        with self._lock:
            if self.offline:
                return
            self.offline = True
            self.offline_reason = reason
            queued, self._queue = self._queue, []
            self._logs = []
        self.warn_once("offline", f"reporting continues offline ({reason}); results go to {self.cfg.spool_dir}")
        for q in queued:
            if not q["running"]:
                self._spool_result(q["result"], [], True)

    def open_running(self, result: Dict[str, Any]) -> None:
        if not self.running or self.offline:
            return
        self._enqueue(dict(result, status="running"), True)

    def log(self, result_id: str, attempt: int, level: str, stream: str, message: str, step: Optional[str] = None) -> None:
        if self.logs_state == "off" or self.offline:
            return
        line = {
            "resultId": result_id,
            "attempt": attempt,
            "ts": int(time.time() * 1000),
            "level": level,
            "stream": stream,
            "message": cut(self.masker.mask(message), MAX_LOG_MESSAGE),
        }
        if step:
            line["step"] = self.masker.mask(step)
        with self._lock:
            key = f"{result_id}\0{attempt}"
            line["seq"] = self._seq.get(key, 0)
            self._seq[key] = line["seq"] + 1
            self._logs.append(line)
            if len(self._logs) > MAX_QUEUED_LOGS:
                del self._logs[: len(self._logs) - MAX_QUEUED_LOGS]
                self.stats["dropped_logs"] += 1

    def _enqueue(self, result: Dict[str, Any], running: bool) -> None:
        size = len(json.dumps(result))
        with self._lock:
            self._queue.append({"running": running, "result": result, "bytes": size})
            if len(self._queue) >= RESULT_BATCH:
                self._wake.notify()

    def finish(self, result: Dict[str, Any], files: List[Dict[str, Any]], passed: bool) -> None:
        """Uploads the attempt's attachments in the background, then queues the final result."""
        rid = result["id"]
        with self._lock:
            self._inflight[rid] = {"result": result, "files": files, "passed": passed}
        try:
            fut = self._pipeline.submit(self._run_pipeline, result, files, passed)
        except RuntimeError:
            return
        fut.add_done_callback(lambda _f: self._done(rid))

    def _done(self, rid: str) -> None:
        with self._lock:
            self._inflight.pop(rid, None)

    def _prepare(self, f: Dict[str, Any]) -> Optional[Dict[str, Any]]:
        try:
            body: Optional[bytes] = f.get("body")
            path: Optional[str] = f.get("path")
            size = len(body) if body is not None else os.path.getsize(path or "")
            if size > self.cfg.max_attachment_bytes:
                self.warn_once(f"big:{f['name']}", f"attachment {f['name']} is too large; skipped")
                return None
            if self.masker.active and is_text(f["contentType"]) and size <= MASK_TEXT_ATTACHMENT_BYTES:
                if body is not None:
                    raw = body
                else:
                    with open(path or "", "rb") as fh:
                        raw = fh.read()
                text = raw.decode("utf-8", "replace")
                masked = self.masker.mask(text)
                if masked != text or body is not None:
                    body, path = masked.encode("utf-8"), None
                    size = len(body)
            sha = hashlib.sha256(body).hexdigest() if body is not None else sha256_file(path or "")
            return {"name": f["name"], "contentType": f["contentType"], "path": path, "body": body, "sha256": sha, "size": size}
        except OSError as e:
            self.warn_once(f"read:{f.get('name')}", f"cannot read attachment {f.get('name')}: {e}")
            return None

    def _upload(self, p: Dict[str, Any]) -> str:
        """An attachment id, ``"skip"`` (rejected for good) or ``"retry"`` (spool it)."""
        with self._lock:
            lock = self._upload_locks.setdefault(p["sha256"], threading.Lock())
        with lock:
            return self._upload_once(p)

    def _upload_once(self, p: Dict[str, Any]) -> str:
        known = self._uploads.get(p["sha256"])
        if known:
            return known
        if self.offline or not self.launch_id:
            return "retry"
        try:
            r = self.api.upload_attachment(
                self.launch_id, p["name"], p["contentType"], path=p["path"], body=p["body"], retries=2
            )
        except ApiError as e:
            if e.fatal and (e.status < 0 or e.status in (400, 413, 415)):
                self.warn_once(f"att:{e.status}", f"attachment {p['name']} rejected: {e}")
                return "skip"
            self._failed(e)
            return "retry"
        self.failures = 0
        self.stats["attachments"] += 1
        self._uploads[p["sha256"]] = r["id"]
        return r["id"]

    def probe_logs(self) -> None:
        """Feature detection: an empty logs request tells whether the server takes live logs."""
        if self.logs_state != "unknown" or self.offline or not self.launch_id:
            return
        try:
            if self.api.post_logs(self.launch_id, [], retries=0) == "unsupported":
                self.logs_state = "off"
            else:
                self.logs_state = "on"
        except ApiError:
            return

    def _apply_fallback_logs(self, result: Dict[str, Any]) -> None:
        """Buffered log lines go into stdout unless the server stores live logs."""
        text = result.pop(FALLBACK_LOGS, None)
        if not text:
            return
        self.probe_logs()
        until = time.time() + LOGS_DETECT_WAIT
        while self.logs_state == "unknown" and not self.offline and time.time() < until:
            time.sleep(0.05)
        self._fold_logs(result, text)

    def _fold_logs(self, result: Dict[str, Any], text: Optional[str] = None) -> None:
        text = text if text is not None else result.pop(FALLBACK_LOGS, None)
        if not text or self.logs_state == "on":
            return
        out = result.get("stdout") or ""
        sep = "\n" if out and not out.endswith("\n") else ""
        result["stdout"] = f"{out}{sep}--- log ---\n{text}\n"

    def _run_pipeline(self, result: Dict[str, Any], files: List[Dict[str, Any]], passed: bool) -> None:
        try:
            prepared = [p for p in (self._prepare(f) for f in files) if p is not None]
            if not self.launch_id and not self.offline:
                self._ensure_launch(1)
            self._apply_fallback_logs(result)
            refs, pending = [], []
            for p in prepared:
                rid = self._upload(p)
                if rid == "retry":
                    pending.append(p)
                elif rid != "skip":
                    refs.append({"id": rid, "name": p["name"], "contentType": p["contentType"]})
            result["attachments"] = list(result.get("attachments") or []) + refs
            if self.offline or pending:
                self._spool_result(result, pending, passed)
                return
            self._enqueue(result, False)
        except Exception as e:
            self.warn_once("pipeline", f"reporting error (ignored): {e}")

    def _spool_result(self, result: Dict[str, Any], blobs: List[Dict[str, Any]], passed: bool) -> None:
        kept, refs = [], list(result.get("attachments") or [])
        for b in blobs:
            if self.spool.add_blob(b["sha256"], b["size"], passed, path=b.get("path"), body=b.get("body")):
                kept.append({"sha256": b["sha256"], "name": b["name"], "contentType": b["contentType"], "size": b["size"]})
                refs.append({"id": BLOB_PREFIX + b["sha256"], "name": b["name"], "contentType": b["contentType"]})
        r = dict(result, attachments=refs)
        if not self.spool.append({"v": SPOOL_VERSION, "result": r, "blobs": kept}):
            self.stats["lost"] += 1

    def _loop(self) -> None:
        while True:
            with self._lock:
                if self._stop:
                    return
                self._wake.wait(self.cfg.batch_interval)
                if self._stop:
                    return
            try:
                self.pump()
            except Exception as e:
                self.warn_once("sender", f"reporting error (ignored): {e}")

    def pump(self) -> None:
        """One round of sending; serialized so results keep their order."""
        with self._send_lock:
            if self.offline:
                return
            if not self.launch_id:
                self._ensure_launch(0)
            if not self.launch_id:
                return
            self.probe_logs()
            self._send_results()
            self._send_logs()

    def _take_batch(self) -> List[Dict[str, Any]]:
        with self._lock:
            batch, size = [], 0
            while self._queue and len(batch) < RESULT_BATCH:
                nxt = self._queue[0]
                if batch and size + nxt["bytes"] > RESULT_BATCH_BYTES:
                    break
                batch.append(self._queue.pop(0))
                size += nxt["bytes"]
            return batch

    def _requeue(self, items: List[Dict[str, Any]]) -> None:
        with self._lock:
            self._queue[:0] = items

    def _send_results(self) -> None:
        while not self.offline:
            batch = self._take_batch()
            if not batch:
                return
            try:
                summary = self.api.post_results(self.launch_id or "", [b["result"] for b in batch], retries=0)
            except ApiError as e:
                if e.status == 413 and len(batch) > 1:
                    for b in batch:
                        b["bytes"] = RESULT_BATCH_BYTES
                    self._requeue(batch)
                    continue
                if e.status == 413:
                    self.stats["lost"] += sum(1 for b in batch if not b["running"])
                    self.warn_once("413", f"a result is too large for the server: {e}")
                    continue
                self._requeue([b for b in batch if not b["running"]])
                self._failed(e)
                return
            self.failures = 0
            rejected = set()
            for r in summary["rejected"]:
                idx = r.get("index", -1)
                rejected.add(idx)
                item = batch[idx] if 0 <= idx < len(batch) else None
                if item is not None and item["running"]:
                    self.running = False
                else:
                    self.stats["lost"] += 1
                    msg = str(r.get("message", ""))
                    self.warn_once(f"rej:{msg[:40]}", f"the server rejected a result: {msg}")
            self.stats["sent"] += sum(1 for i, b in enumerate(batch) if not b["running"] and i not in rejected)

    def _send_logs(self) -> None:
        while not self.offline and self.logs_state != "off":
            with self._lock:
                batch, self._logs = self._logs[:1000], self._logs[1000:]
            if not batch:
                return
            try:
                r = self.api.post_logs(self.launch_id or "", batch, retries=0)
            except ApiError as e:
                if e.fatal:
                    self.warn_once("logs", f"live logs rejected ({e}); they stay in the results")
                    self.logs_state = "off"
                    with self._lock:
                        self._logs = []
                    return
                with self._lock:
                    self._logs[:0] = batch
                self._failed(e)
                return
            if r == "unsupported":
                self.logs_state = "off"
                with self._lock:
                    self._logs = []
                return
            self.logs_state = "on"
            self.failures = 0

    def _spool_inflight(self) -> None:
        with self._lock:
            inflight, self._inflight = list(self._inflight.values()), {}
        for f in inflight:
            self._fold_logs(f["result"])
            blobs = [p for p in (self._prepare(x) for x in f["files"]) if p is not None]
            self._spool_result(f["result"], blobs, f["passed"])

    def close(self, status: str = "finished") -> Dict[str, Any]:
        """Drains uploads and queues within the flush timeout, spools the rest, finishes the launch when
        asked, and returns a summary. Never raises."""
        deadline = time.time() + self.cfg.flush_timeout

        def remaining() -> float:
            return max(0.0, deadline - time.time())

        try:
            with self._lock:
                self._stop = True
                self._wake.notify_all()
            if self._thread:
                self._thread.join(remaining())
            self._pipeline.shutdown(wait=False)
            while remaining() > 0:
                with self._lock:
                    busy = bool(self._inflight)
                if not busy:
                    break
                time.sleep(0.02)
            while not self.offline and remaining() > 0:
                with self._lock:
                    before = len(self._queue) + len(self._logs)
                if not before:
                    break
                self.pump()
                with self._lock:
                    after = len(self._queue) + len(self._logs)
                if after >= before:
                    time.sleep(min(0.2, remaining()))
            if self._old_spool:
                self._old_spool.join(remaining())
            with self._lock:
                leftover = bool(self._inflight) or any(not q["running"] for q in self._queue)
            if leftover:
                self._go_offline(self.offline_reason or "the flush timeout was reached")
                self._spool_inflight()
                with self._lock:
                    queued, self._queue = self._queue, []
                for q in queued:
                    if not q["running"]:
                        self._spool_result(q["result"], [], True)
            want_finish = self.cfg.finish_launch == "always" or (self.cfg.finish_launch == "auto" and self.cfg.own_launch)
            if self.spool.opened:
                self.spool.end(status if want_finish else None)
                if not self.cfg.offline and remaining() > 0:
                    try:
                        r = flush_spool(self.api, self.cfg.spool_dir, deadline=deadline, project=self.cfg.project)
                        self.stats["sent"] += r["results"]
                    except Exception:
                        pass
            elif want_finish and self.launch_id and remaining() > 0:
                try:
                    self.api.finish_launch(self.launch_id, status, retries=1)
                except ApiError as e:
                    self._failed(e)
        except Exception as e:
            self.warn_once("close", f"reporting error (ignored): {e}")
        spooled_left = self.spool.opened and os.path.isdir(self.spool.dir)
        return {
            "launchId": self.launch_id,
            "launchNumber": self.launch_number,
            "sent": self.stats["sent"],
            "spooled": self.spool.written["results"] if spooled_left else 0,
            "lost": self.stats["lost"] + self.spool.dropped["results"],
            "attachments": self.stats["attachments"],
            "offline": self.offline,
            "offlineReason": self.offline_reason,
            "spoolDir": self.spool.dir if spooled_left else None,
            "logs": self.logs_state,
            "running": self.running,
        }
