"""pytest plugin that mirrors allure-pytest results to FastProof while the tests run.

It never replaces the Allure writer: allure-results are written exactly as before. Two pieces cooperate:

* ``Enhancer`` (a pytest plugin) numbers attempts (one per ``pytest_runtest_logstart``, which covers
  pytest-rerunfailures), opens each as a ``running`` result, adds the ``traceparent`` label to the Allure
  result and streams log records;
* ``AllureHooks`` (registered in ``allure_commons.plugin_manager``) sees the finished Allure ``TestResult``
  (``report_result``), its attachment files and the live ``start_step`` / ``stop_step`` events.

Reporting problems never reach the tests or change the exit code.
"""

from __future__ import annotations

import hashlib
import logging
import os
import secrets
import shutil
import time
from collections import deque
from typing import Any, Deque, Dict, List, Optional

import pytest

import allure_commons
from pytest_fastproof.client import Client, cut
from pytest_fastproof.config import Config
from pytest_fastproof.plugin import _resolve
from pytest_fastproof.trace import TraceContext

from .mapping import to_report

WORKERINPUT = "fastproof_allure_pytest"
STEP_SEP = " › "
_LEVELS = ((40, "error"), (30, "warn"), (20, "info"), (10, "debug"))
TRACE_LABEL = "traceparent"

_current: Optional["Attempt"] = None


def current_trace() -> Optional[TraceContext]:
    return _current.trace if _current is not None else None


def _now_ms() -> int:
    return int(time.time() * 1000)


def _level(no: int) -> str:
    for threshold, name in _LEVELS:
        if no >= threshold:
            return name
    return "trace"


def result_id(run_id: str, nodeid: str, attempt: int) -> str:
    return f"ap-{run_id}-{hashlib.sha1(nodeid.encode()).hexdigest()[:16]}-{attempt}"


class Attempt:
    def __init__(self, nodeid: str, attempt: int, rid: str, trace: TraceContext, item: Any) -> None:
        self.nodeid = nodeid
        self.attempt = attempt
        self.id = rid
        self.trace = trace
        self.item = item
        self.stack: List[str] = []
        self.outcomes: Dict[str, str] = {}
        self.error = ""
        self.start = _now_ms()


class LogHandler(logging.Handler):
    def __init__(self, owner: "Enhancer", level: int) -> None:
        super().__init__(level)
        self.owner = owner

    def emit(self, record: logging.LogRecord) -> None:
        att = self.owner.attempt
        if att is None or record.name.startswith(("pytest_fastproof", "fastproof_allure_pytest")):
            return
        try:
            msg = record.getMessage()
            if record.exc_info and not record.exc_text:
                record.exc_text = logging.Formatter().formatException(record.exc_info)
            if record.exc_text:
                msg = f"{msg}\n{record.exc_text}"
            self.owner.log(att, _level(record.levelno), "log", f"{record.name}: {msg}")
        except Exception:
            self.handleError(record)


class AllureHooks:
    """Registered in ``allure_commons.plugin_manager``; the hooks only collect or enqueue."""

    def __init__(self, owner: "Enhancer") -> None:
        self.owner = owner

    @allure_commons.hookimpl
    def start_step(self, uuid, title, params):
        self.owner.guard(self.owner.step_started, title)

    @allure_commons.hookimpl
    def stop_step(self, uuid, exc_type, exc_val, exc_tb):
        self.owner.guard(self.owner.step_stopped, exc_type, exc_val)

    @allure_commons.hookimpl
    def report_attached_file(self, source, file_name):
        self.owner.guard(self.owner.file_attached, file_name, source, None)

    @allure_commons.hookimpl
    def report_attached_data(self, body, file_name):
        self.owner.guard(self.owner.file_attached, file_name, None, body)

    @allure_commons.hookimpl
    def report_result(self, result):
        self.owner.guard(self.owner.result_ready, result)


class Enhancer:
    def __init__(self, config: pytest.Config) -> None:
        self.config = config
        self.cfg: Optional[Config] = None
        self.client: Optional[Client] = None
        self.run_id = secrets.token_hex(6)
        self.is_worker = hasattr(config, "workerinput")
        self.controller = False
        self.attempt: Optional[Attempt] = None
        self.counts: Dict[str, int] = {}
        self.items: Dict[str, Any] = {}
        self.awaiting: Deque[Attempt] = deque()
        self.uploads: Dict[str, Dict[str, Any]] = {}
        self.staging = ""
        self.handler: Optional[LogHandler] = None
        self.hooks: Optional[AllureHooks] = None
        self.message = ""
        self.summary: Optional[Dict[str, Any]] = None
        self.trace_label = True
        self.missing = 0

    def setup(self) -> None:
        config = self.config
        if not config.getoption("allure_report_dir", default=None):
            return
        self.controller = not self.is_worker and config.getoption("dist", default="no") not in (None, "no")
        cfg = _resolve(config)
        self.cfg = cfg
        if self.is_worker:
            wi = config.workerinput.get(WORKERINPUT)
            if not wi:
                return
            cfg.launch = wi["launch"]
            cfg.own_launch = False
            cfg.finish_launch = "never"
            self.run_id = wi["runId"]
        if not cfg.enabled:
            if cfg.reason not in ("disabled", "FASTPROOF_URL is not set") and not self.is_worker:
                self.message = f"reporting is off: {cfg.reason}"
            return
        self.trace_label = os.environ.get("FASTPROOF_ALLURE_TRACE_LABEL", "").strip().lower() not in ("0", "false", "no", "off")
        self.staging = os.path.join(cfg.spool_dir, ".staging", self.run_id, f"w{os.getpid()}")
        self.client = Client(cfg, role="worker" if self.is_worker else "reporter")
        self.client.start(flush_old=not self.is_worker)
        if self.controller:
            if config.pluginmanager.hasplugin("xdist"):
                config.pluginmanager.register(XdistHooks(self), "fastproof-allure-xdist")
            return
        self.hooks = AllureHooks(self)
        allure_commons.plugin_manager.register(self.hooks, "fastproof-allure-hooks")
        self.handler = LogHandler(self, cfg.log_level)
        root = logging.getLogger()
        root.addHandler(self.handler)
        if root.level > cfg.log_level or root.level == logging.NOTSET:
            root.setLevel(cfg.log_level)

    def guard(self, fn, *args) -> None:
        try:
            fn(*args)
        except Exception as e:
            if self.client is not None:
                self.client.warn_once(f"hook:{type(e).__name__}", f"reporter error (ignored): {e}")

    def mask(self, s: Any) -> Any:
        return self.client.masker.mask(s) if self.client is not None else s

    def log(self, att: Attempt, level: str, stream: str, message: str, step: Optional[str] = None) -> None:
        if self.client is not None:
            self.client.log(att.id, att.attempt, level, stream, message, step)

    def begin(self, nodeid: str) -> None:
        global _current
        self.flush_stale()
        n = self.counts.get(nodeid, 0)
        self.counts[nodeid] = n + 1
        item = self.items.get(nodeid)
        att = Attempt(
            nodeid, n, result_id(self.run_id, nodeid, n),
            TraceContext.new(self.cfg.launch.get("externalId", ""), nodeid, n), item,
        )
        self.attempt = _current = att
        if self.client is None:
            return
        r: Dict[str, Any] = {"id": att.id, "name": self.mask(nodeid.split("::")[-1]), "status": "running", "start": att.start, "retry": n}
        try:
            r.update(self.identity(item))
        except Exception:
            pass
        self.client.open_running(r)

    def identity(self, item: Any) -> Dict[str, Any]:
        """The identity the final line will have (allure-pytest computes it the same way)."""
        if item is None:
            return {}
        from allure_commons.utils import md5, represent
        from allure_pytest.utils import allure_full_name, allure_name

        spec = getattr(item, "callspec", None)
        params = dict(spec.params) if spec is not None else {}
        full = allure_full_name(item)
        out: Dict[str, Any] = {
            "name": self.mask(allure_name(item, params, spec.id if spec is not None else None)),
            "fullName": self.mask(full),
            "testCaseId": md5(full),
            "parameters": [{"name": str(k), "value": self.mask(cut(represent(v), 1000))} for k, v in params.items()],
        }
        fspath, line, _ = item.location
        out["source"] = {"file": fspath, "line": (line or 0) + 1}
        return out

    def label_trace(self, item: Any) -> None:
        att = self.attempt
        if att is None or not self.trace_label:
            return
        allure_commons.plugin_manager.hook.add_label(label_type=TRACE_LABEL, labels=[att.trace.traceparent])

    def step_started(self, title: str) -> None:
        att = self.attempt
        if att is None:
            return
        att.stack.append(str(title))
        self.log(att, "info", "step", f"▶ {title}", STEP_SEP.join(att.stack))

    def step_stopped(self, exc_type: Any, exc_val: Any) -> None:
        att = self.attempt
        if att is None or not att.stack:
            return
        title, path = att.stack[-1], STEP_SEP.join(att.stack)
        att.stack.pop()
        if exc_type is not None:
            self.log(att, "error", "step", f"✘ {title}: {exc_type.__name__}: {exc_val}", path)
        else:
            self.log(att, "info", "step", f"✔ {title}", path)

    def file_attached(self, file_name: str, source: Optional[str], body: Any) -> None:
        if self.client is None:
            return
        if body is not None:
            data = body.encode("utf-8") if isinstance(body, str) else bytes(body)
            sha = hashlib.sha256(data).hexdigest()
            os.makedirs(self.staging, exist_ok=True)
            target = os.path.join(self.staging, sha)
            if not os.path.exists(target):
                tmp = f"{target}.tmp"
                with open(tmp, "wb") as f:
                    f.write(data)
                os.replace(tmp, target)
            source = target
        if source:
            self.uploads[file_name] = {"path": os.path.abspath(source)}

    def result_ready(self, result: Any) -> None:
        if self.client is None:
            return
        att = self.awaiting.popleft() if self.awaiting else None
        if att is None:
            self.missing += 1
            return
        item = att.item
        source = None
        if item is not None:
            fspath, line, _ = item.location
            source = (fspath, (line or 0) + 1)
        tp = att.trace.traceparent
        r, files, used = to_report(result, att.id, att.attempt, tp, self.mask, self.uploads, source)
        for source_name in used:
            self.uploads.pop(source_name, None)
        self.client.finish(r, files, r["status"] in ("passed", "skipped"))

    def flush_stale(self) -> None:
        """Attempts that never got an Allure result (the Allure listener was off) are closed from pytest's
        own outcome, so no result stays ``running``."""
        while self.awaiting:
            att = self.awaiting.popleft()
            self.missing += 1
            self.close_from_pytest(att)

    def close_from_pytest(self, att: Attempt) -> None:
        if self.client is None:
            return
        outcomes = set(att.outcomes.values())
        status = "broken" if "failed" in outcomes else "skipped" if "skipped" in outcomes and "passed" not in outcomes else "passed"
        r: Dict[str, Any] = {
            "id": att.id, "name": self.mask(att.nodeid.split("::")[-1]), "status": status, "retry": att.attempt,
            "start": att.start, "stop": _now_ms(), "traceparent": att.trace.traceparent,
        }
        if att.error:
            r["error"] = {"message": self.mask(cut(att.error, 64 * 1024))}
        try:
            r.update({k: v for k, v in self.identity(att.item).items() if k != "name"})
        except Exception:
            pass
        self.client.finish(r, [], status in ("passed", "skipped"))

    def logreport(self, rep: Any) -> None:
        att = self.attempt
        if att is None or rep.when is None:
            return
        att.outcomes[rep.when] = rep.outcome
        if rep.outcome == "failed" and not att.error:
            lines = str(getattr(rep, "longreprtext", "") or "").strip().splitlines()
            att.error = lines[-1] if lines else ""

    def logfinish(self, nodeid: str) -> None:
        global _current
        att, self.attempt, _current = self.attempt, None, None
        if att is not None and att.nodeid == nodeid:
            self.awaiting.append(att)

    def finish(self, exitstatus: int) -> None:
        if self.hooks is not None:
            self.guard(self.flush_stale)
            try:
                allure_commons.plugin_manager.unregister(self.hooks)
            except Exception:
                pass
        if self.handler is not None:
            logging.getLogger().removeHandler(self.handler)
            self.handler = None
        client = self.client
        if client is None:
            return
        if self.is_worker:
            client.cfg.flush_timeout = min(client.cfg.flush_timeout, 10.0)
            client.close("finished")
        else:
            interrupted = exitstatus in (pytest.ExitCode.INTERRUPTED, pytest.ExitCode.INTERNAL_ERROR)
            if self.controller:
                self.guard(client._ensure_launch, 1)
            self.summary = client.close("interrupted" if interrupted else "finished")
        if self.staging:
            shutil.rmtree(self.staging, ignore_errors=True)
            try:
                os.rmdir(os.path.dirname(self.staging))
            except OSError:
                pass


class XdistHooks:
    def __init__(self, owner: Enhancer) -> None:
        self.owner = owner

    def pytest_configure_node(self, node: Any) -> None:
        o = self.owner
        if o.cfg is not None and o.cfg.enabled:
            node.workerinput[WORKERINPUT] = {"launch": o.cfg.launch, "runId": o.run_id}


class EnhancerPlugin:
    def __init__(self, enhancer: Enhancer) -> None:
        self.e = enhancer

    def pytest_collection_finish(self, session: pytest.Session) -> None:
        self.e.items = {it.nodeid: it for it in session.items}

    @pytest.hookimpl(tryfirst=True)
    def pytest_runtest_logstart(self, nodeid: str, location: Any) -> None:
        if self.e.client is not None and not self.e.controller:
            self.e.guard(self.e.begin, nodeid)

    @pytest.hookimpl(tryfirst=True)
    def pytest_runtest_setup(self, item: pytest.Item) -> None:
        if self.e.client is not None and not self.e.controller:
            self.e.guard(self.e.label_trace, item)

    def pytest_runtest_logreport(self, report: pytest.TestReport) -> None:
        if self.e.client is not None and not self.e.controller:
            self.e.guard(self.e.logreport, report)

    @pytest.hookimpl(trylast=True)
    def pytest_runtest_logfinish(self, nodeid: str, location: Any) -> None:
        if self.e.client is not None and not self.e.controller:
            self.e.guard(self.e.logfinish, nodeid)

    @pytest.hookimpl(trylast=True)
    def pytest_sessionfinish(self, session: pytest.Session, exitstatus: int) -> None:
        self.e.guard(self.e.finish, exitstatus)

    def pytest_terminal_summary(self, terminalreporter: Any) -> None:
        e = self.e
        if e.message:
            terminalreporter.write_line(f"[fastproof] {e.message}")
        s = e.summary
        if not s or e.cfg is None:
            return
        where = f"launch #{s['launchNumber']}" if s.get("launchNumber") is not None else "launch"
        if s["spooled"]:
            reason = f" ({s['offlineReason']})" if s.get("offlineReason") else ""
            terminalreporter.write_line(f"[fastproof] {s['sent']} results sent, {s['spooled']} saved to {s['spoolDir']}{reason}")
            terminalreporter.write_line(
                f"[fastproof] the next run sends them, or now: python -m pytest_fastproof flush --spool {e.cfg.spool_dir}"
                " (with FASTPROOF_URL, FASTPROOF_TOKEN and FASTPROOF_PROJECT set)"
            )
        elif not e.cfg.quiet:
            terminalreporter.write_line(f"[fastproof] {where}: {s['sent']} results, {s['attachments']} attachments sent")
        if s["lost"]:
            terminalreporter.write_line(f"[fastproof] {s['lost']} results could not be reported")
        if e.missing:
            terminalreporter.write_line(f"[fastproof] {e.missing} attempts had no Allure result; they were reported from pytest's own outcome")


def pytest_configure(config: pytest.Config) -> None:
    enhancer = Enhancer(config)
    try:
        enhancer.setup()
    except Exception as e:
        enhancer.message = f"reporting is off: {e}"
    config.pluginmanager.register(EnhancerPlugin(enhancer), "fastproof-allure-pytest-plugin")


@pytest.fixture
def fastproof_trace() -> TraceContext:
    """Trace context of the current attempt (``traceparent``, ``baggage``, ``headers()``, ``env()``)."""
    return _current.trace if _current is not None else TraceContext.new("", "", 0)
