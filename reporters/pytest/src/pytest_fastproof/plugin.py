"""pytest hooks. Two roles: the *executor* runs tests (a worker, or the only process) and adds FastProof data
to each phase report, streams live logs and collects attachments; the *reporter* (the controller, or the
only process) turns phase reports into attempts and sends them. Under xdist only the controller sends
results, so nothing is reported twice."""

from __future__ import annotations

import hashlib
import logging
import os
import secrets
import shutil
import threading
import time
from contextlib import contextmanager
from typing import Any, Dict, Iterator, List, Optional

import pytest

from .client import FALLBACK_LOGS, Client, cut
from .config import Config, resolve_config
from .testplan import TestPlan, load_test_plan, strip_params
from .trace import TraceContext

MAX_OUTPUT = 4 * 1024 * 1024
MAX_FALLBACK_LOG = 1024 * 1024
BUILTIN_MARKERS = {
    "parametrize", "usefixtures", "filterwarnings", "skip", "skipif", "xfail", "tryfirst", "trylast",
    "flaky", "fastproof_id", "fastproof_label", "fastproof_link", "allure_label", "allure_link",
}
_LEVELS = ((40, "error"), (30, "warn"), (20, "info"), (10, "debug"))

_executor: Optional["Executor"] = None


def _now_ms() -> int:
    return int(time.time() * 1000)


def result_id(run_id: str, nodeid: str, attempt: int) -> str:
    return f"py-{run_id}-{hashlib.sha1(nodeid.encode()).hexdigest()[:16]}-{attempt}"


def _level(no: int) -> str:
    for threshold, name in _LEVELS:
        if no >= threshold:
            return name
    return "trace"


class Attempt:
    def __init__(self, nodeid: str, attempt: int, rid: str, trace: TraceContext) -> None:
        self.nodeid = nodeid
        self.attempt = attempt
        self.id = rid
        self.trace = trace
        self.files: List[Dict[str, Any]] = []
        self.log_lines: List[str] = []
        self.log_bytes = 0
        self.steps: List[Dict[str, Any]] = []
        self.stack: List[Dict[str, Any]] = []


class LogHandler(logging.Handler):
    """Streams log records of the running test live and keeps a bounded copy for servers without logs."""

    def __init__(self, executor: "Executor", level: int) -> None:
        super().__init__(level)
        self.executor = executor

    def emit(self, record: logging.LogRecord) -> None:
        att = self.executor.current
        if att is None or record.name.startswith("pytest_fastproof"):
            return
        try:
            msg = record.getMessage()
            if record.exc_info and not record.exc_text:
                record.exc_text = logging.Formatter().formatException(record.exc_info)
            if record.exc_text:
                msg = f"{msg}\n{record.exc_text}"
            self.executor.log(att, _level(record.levelno), "log", f"{record.name}: {msg}", record.levelname)
        except Exception:
            self.handleError(record)


def item_identity(item: Any, mask: Any) -> Dict[str, Any]:
    """Parameters and explicit test case id of an item: the parts of the identity a running line must
    carry so the final line updates the same result."""
    test_case_id = None
    for mk in item.iter_markers():
        if mk.name == "fastproof_id" and mk.args:
            test_case_id = str(mk.args[0])
        elif mk.name == "allure_label" and mk.kwargs.get("label_type") in ("as_id", "ALLURE_ID") and mk.args:
            test_case_id = test_case_id or str(mk.args[0])
    params = []
    callspec = getattr(item, "callspec", None)
    for k, v in (getattr(callspec, "params", None) or {}).items():
        params.append({"name": str(k), "value": mask(cut(v if isinstance(v, str) else repr(v), 1000))})
    return {"parameters": params, "testCaseId": test_case_id}


class Executor:
    """Per test process: the current attempt, its trace context, attachments, steps and logs."""

    def __init__(self, cfg: Config, run_id: str, client: Optional[Client], staging: str) -> None:
        self.cfg = cfg
        self.run_id = run_id
        self.client = client
        self.staging = staging
        self.current: Optional[Attempt] = None
        self.counts: Dict[str, int] = {}
        self.worker = os.environ.get("PYTEST_XDIST_WORKER", "main")
        self.handler: Optional[LogHandler] = None
        self._lock = threading.Lock()

    def install_handler(self) -> None:
        if self.client is None:
            return
        self.handler = LogHandler(self, self.cfg.log_level)
        root = logging.getLogger()
        root.addHandler(self.handler)
        if root.level > self.cfg.log_level or root.level == logging.NOTSET:
            root.setLevel(self.cfg.log_level)

    def remove_handler(self) -> None:
        if self.handler is not None:
            logging.getLogger().removeHandler(self.handler)
            self.handler = None

    def begin(self, nodeid: str) -> Attempt:
        n = self.counts.get(nodeid, 0)
        self.counts[nodeid] = n + 1
        att = Attempt(
            nodeid, n, result_id(self.run_id, nodeid, n),
            TraceContext.new(self.cfg.launch.get("externalId", ""), nodeid, n),
        )
        self.current = att
        return att

    def log(self, att: Attempt, level: str, stream: str, message: str, levelname: str = "INFO", step: Optional[str] = None) -> None:
        if self.client is not None:
            self.client.log(att.id, att.attempt, level, stream, message, step)
            message = self.client.masker.mask(message)
        if att.log_bytes < MAX_FALLBACK_LOG:
            stamp = time.strftime("%H:%M:%S", time.localtime()) + f".{int(time.time() * 1000) % 1000:03d}"
            line = f"{stamp} {levelname:<7} {message}"
            att.log_lines.append(line)
            att.log_bytes += len(line) + 1

    def attach(self, name: str, content_type: str, body: Optional[bytes] = None, path: Optional[str] = None) -> None:
        att = self.current
        if att is None:
            return
        if body is not None:
            sha = hashlib.sha256(body).hexdigest()
            os.makedirs(self.staging, exist_ok=True)
            target = os.path.join(self.staging, sha)
            if not os.path.exists(target):
                tmp = f"{target}.{os.getpid()}.tmp"
                with open(tmp, "wb") as f:
                    f.write(body)
                os.replace(tmp, target)
            path = target
        if path is None:
            return
        att.files.append({"name": name, "contentType": content_type, "path": os.path.abspath(path)})

    @contextmanager
    def step(self, name: str) -> Iterator[None]:
        att = self.current
        if att is None:
            yield
            return
        node: Dict[str, Any] = {"name": name, "status": "passed", "start": _now_ms(), "steps": []}
        (att.stack[-1]["steps"] if att.stack else att.steps).append(node)
        att.stack.append(node)
        path = " › ".join(n["name"] for n in att.stack)
        self.log(att, "info", "step", f"▶ {name}", step=path)
        try:
            yield
        except BaseException as e:
            node["status"] = "failed" if isinstance(e, AssertionError) else "broken"
            node["error"] = {"message": f"{type(e).__name__}: {e}"}
            self.log(att, "error", "step", f"✘ {name}: {type(e).__name__}: {e}", "ERROR", step=path)
            raise
        finally:
            node["stop"] = _now_ms()
            node["durationMs"] = node["stop"] - node["start"]
            att.stack.pop()
            if node["status"] == "passed":
                self.log(att, "info", "step", f"✔ {name} ({node['durationMs']} ms)", step=path)

    def meta(self, item: pytest.Item, att: Attempt, when: str) -> Dict[str, Any]:
        m = self.client.masker if self.client is not None else None

        def mask(s: str) -> str:
            return m.mask(s) if m is not None else s

        labels = [{"name": "framework", "value": "pytest"}, {"name": "thread", "value": self.worker}]
        nodeid = item.nodeid
        parts = strip_params(nodeid).split("::")
        labels.append({"name": "suite", "value": parts[0]})
        if len(parts) > 2:
            labels.append({"name": "subSuite", "value": "::".join(parts[1:-1])})
        links: List[Dict[str, Any]] = []
        ident = item_identity(item, mask)
        tags = []
        for mk in item.iter_markers():
            if mk.name == "fastproof_id" or (mk.name == "allure_label" and mk.kwargs.get("label_type") in ("as_id", "ALLURE_ID")):
                continue
            elif mk.name == "fastproof_label" and len(mk.args) >= 2:
                labels.append({"name": str(mk.args[0]), "value": mask(str(mk.args[1]))})
            elif mk.name == "fastproof_link" and mk.args:
                link = {"url": str(mk.args[0]), "type": str(mk.kwargs.get("type", "link"))}
                if mk.kwargs.get("name"):
                    link["name"] = str(mk.kwargs["name"])
                links.append(link)
            elif mk.name not in BUILTIN_MARKERS and mk.name not in tags:
                tags.append(mk.name)
        labels.extend({"name": "tag", "value": t} for t in tags)
        fspath, line, _ = item.location
        meta: Dict[str, Any] = {
            "v": 1,
            "id": att.id,
            "attempt": att.attempt,
            "name": mask(item.name),
            "fullName": mask(strip_params(nodeid)),
            "labels": labels,
            "links": links,
            "parameters": ident["parameters"],
            "testCaseId": ident["testCaseId"],
            "source": {"file": fspath, "line": (line or 0) + 1},
            "traceparent": att.trace.traceparent,
        }
        if when == "teardown":
            meta["files"] = [dict(f, name=mask(f["name"])) for f in att.files]
            meta["steps"] = att.steps
            meta["logs"] = "\n".join(att.log_lines)
        return meta


def _current() -> Optional[Attempt]:
    return _executor.current if _executor is not None else None


class OpenAttempt:
    def __init__(self, nodeid: str, attempt: int, rid: str, location: Any) -> None:
        self.nodeid = nodeid
        self.attempt = attempt
        self.id = rid
        self.location = location
        self.opened = _now_ms()
        self.phases: Dict[str, Any] = {}
        self.meta: Dict[str, Any] = {}


def _crash_message(rep: Any) -> str:
    lr = getattr(rep, "longrepr", None)
    crash = getattr(lr, "reprcrash", None)
    if crash is not None and getattr(crash, "message", None):
        return str(crash.message)
    if isinstance(lr, tuple) and len(lr) == 3:
        return str(lr[2])
    text = str(getattr(rep, "longreprtext", "") or lr or "")
    return text.strip().splitlines()[-1] if text.strip() else ""


def _is_assertion(message: str) -> bool:
    return message.startswith(("assert ", "AssertionError", "Failed:", "[XPASS(strict)]")) or message == "assert"


def attempt_status(phases: Dict[str, Any]):
    """The status of one attempt and the report that explains it."""
    setup, call, teardown = phases.get("setup"), phases.get("call"), phases.get("teardown")
    others = [r for w, r in phases.items() if w not in ("setup", "call", "teardown")]

    def failed(r: Any) -> bool:
        return r is not None and r.outcome in ("failed", "rerun")

    for r in others:
        if failed(r):
            return "broken", r
    if failed(setup):
        return "broken", setup
    if setup is not None and setup.outcome == "skipped":
        return "skipped", setup
    if call is not None:
        if failed(call):
            return ("failed" if _is_assertion(_crash_message(call)) else "broken"), call
        if call.outcome == "skipped":
            return "skipped", call
    if failed(teardown):
        return "broken", teardown
    if call is None and setup is None and not others:
        return "unknown", None
    return "passed", None


class Reporter:
    """Turns phase reports into attempts; one per logstart/logfinish pair (pytest-rerunfailures included)."""

    def __init__(self, cfg: Config, run_id: str, client: Client) -> None:
        self.cfg = cfg
        self.run_id = run_id
        self.client = client
        self.counts: Dict[str, int] = {}
        self.open: Dict[str, OpenAttempt] = {}
        self.sections: Dict[str, int] = {}
        self.items: Dict[str, Any] = {}
        self.summary: Optional[Dict[str, Any]] = None
        self.failed_any = False

    def logstart(self, nodeid: str, location: Any) -> None:
        n = self.counts.get(nodeid, 0)
        self.counts[nodeid] = n + 1
        oa = OpenAttempt(nodeid, n, result_id(self.run_id, nodeid, n), location)
        self.open[nodeid] = oa
        m = self.client.masker
        parts = strip_params(nodeid).split("::")
        r = {
            "id": oa.id,
            "name": m.mask(nodeid.split("::")[-1]),
            "fullName": m.mask(strip_params(nodeid)),
            "status": "running",
            "start": oa.opened,
            "retry": n,
            "labels": [{"name": "framework", "value": "pytest"}, {"name": "suite", "value": parts[0]}],
        }
        if location:
            r["source"] = {"file": location[0], "line": (location[1] or 0) + 1}
        item = self.items.get(nodeid)
        if item is not None:
            # The final line carries the same identity, so it completes this result in place.
            ident = item_identity(item, m.mask)
            r["parameters"] = ident["parameters"]
            if ident["testCaseId"]:
                r["testCaseId"] = ident["testCaseId"]
        self.client.open_running(r)

    def logreport(self, rep: Any) -> None:
        oa = self.open.get(rep.nodeid)
        if oa is None:
            self.logstart(rep.nodeid, getattr(rep, "location", None))
            oa = self.open[rep.nodeid]
        oa.phases[rep.when or "???"] = rep
        meta = getattr(rep, "fastproof", None)
        if isinstance(meta, dict):
            oa.meta.update(meta)

    def logfinish(self, nodeid: str) -> None:
        oa = self.open.pop(nodeid, None)
        if oa is not None:
            self.finalize(oa)

    def finalize(self, oa: OpenAttempt, interrupted: bool = False) -> None:
        m = self.client.masker
        meta = oa.meta
        status, why = attempt_status(oa.phases)
        if interrupted and status in ("passed", "unknown"):
            status = "broken"
        rid = meta.get("id") or oa.id
        attempt = meta.get("attempt", oa.attempt)
        starts = [getattr(r, "start", None) for r in oa.phases.values()]
        stops = [getattr(r, "stop", None) for r in oa.phases.values()]
        start = int(min(s for s in starts if s) * 1000) if any(starts) else oa.opened
        stop = int(max(s for s in stops if s) * 1000) if any(stops) else _now_ms()
        parts = strip_params(oa.nodeid).split("::")
        r: Dict[str, Any] = {
            "id": rid,
            "name": meta.get("name") or m.mask(oa.nodeid.split("::")[-1]),
            "fullName": meta.get("fullName") or m.mask(strip_params(oa.nodeid)),
            "status": status,
            "start": start,
            "stop": max(stop, start),
            "durationMs": max(0, stop - start),
            "retry": attempt,
            "labels": meta.get("labels") or [{"name": "framework", "value": "pytest"}, {"name": "suite", "value": parts[0]}],
            "parameters": meta.get("parameters") or [],
        }
        for k in ("links", "testCaseId", "source", "traceparent", "steps"):
            if meta.get(k):
                r[k] = meta[k]
        if not r.get("source") and oa.location:
            r["source"] = {"file": oa.location[0], "line": (oa.location[1] or 0) + 1}
        if why is not None:
            msg = _crash_message(why)
            if status == "skipped":
                wasx = getattr(why, "wasxfail", None)
                msg = f"xfail: {wasx}" if wasx is not None else msg.replace("Skipped: ", "", 1)
                r["error"] = {"message": m.mask(cut(msg, 64 * 1024))}
            else:
                r["error"] = {
                    "message": m.mask(cut(msg, 64 * 1024)),
                    "trace": m.mask(cut(str(getattr(why, "longreprtext", "") or ""), MAX_OUTPUT)),
                }
        elif interrupted:
            r["error"] = {"message": "the test did not finish (worker crash or interrupted run)"}
        stdout, stderr = self.output(oa)
        if meta.get("logs"):
            r[FALLBACK_LOGS] = m.mask(cut(meta["logs"], MAX_FALLBACK_LOG))
        if stdout:
            r["stdout"] = m.mask(cut(stdout, MAX_OUTPUT))
        if stderr:
            r["stderr"] = m.mask(cut(stderr, MAX_OUTPUT))
        files = [
            {"name": f["name"], "contentType": f["contentType"], "path": f["path"]}
            for f in meta.get("files") or []
            if isinstance(f, dict) and f.get("path")
        ]
        if status in ("failed", "broken"):
            self.failed_any = True
        self.client.finish(r, files, status in ("passed", "skipped"))

    def output(self, oa: OpenAttempt):
        """Captured stdout and stderr of this attempt. pytest keeps appending sections to the item across
        phases and reruns, so the last report holds everything; earlier attempts' sections are skipped."""
        reps = [oa.phases[w] for w in ("setup", "call", "teardown") if w in oa.phases]
        if not reps:
            return "", ""
        sections = list(getattr(reps[-1], "sections", []) or [])
        seen = self.sections.get(oa.nodeid, 0)
        self.sections[oa.nodeid] = len(sections)
        out, err = [], []
        for title, content in sections[seen:] if seen <= len(sections) else sections:
            if title.startswith("Captured stdout"):
                out.append(content)
            elif title.startswith("Captured stderr"):
                err.append(content)
        return "".join(out), "".join(err)

    def finish_open(self) -> None:
        for oa in list(self.open.values()):
            self.finalize(oa, interrupted=True)
        self.open.clear()


def _opt(config: pytest.Config, name: str, ini: Optional[str] = None) -> Any:
    v = config.getoption(name, default=None)
    if v is None and ini:
        try:
            v = config.getini(ini)
        except (ValueError, KeyError):
            v = None
    if v in ("", []):
        return None
    return v


def pytest_addoption(parser: pytest.Parser) -> None:
    g = parser.getgroup("fastproof", "FastProof reporting")
    g.addoption("--fastproof-url", dest="fastproof_url", help="FastProof server URL (FASTPROOF_URL)")
    g.addoption("--fastproof-project", dest="fastproof_project", help="project key (FASTPROOF_PROJECT)")
    g.addoption("--fastproof-launch", dest="fastproof_launch", help="launch external id (FASTPROOF_LAUNCH)")
    g.addoption("--fastproof-launch-name", dest="fastproof_launch_name", help="launch name")
    g.addoption("--fastproof-environment", dest="fastproof_environment", help="environment key")
    g.addoption("--fastproof-attr", dest="fastproof_attrs", action="append", default=[], metavar="KEY=VALUE", help="launch attribute")
    g.addoption("--fastproof-testplan", dest="fastproof_testplan", help="Allure-format testplan.json; tests not in it are deselected")
    g.addoption("--fastproof-spool-dir", dest="fastproof_spool_dir", help="offline spool directory (default .fastproof/spool)")
    g.addoption("--fastproof-offline", dest="fastproof_offline", action="store_true", default=None, help="only write the spool")
    g.addoption("--fastproof-disable", dest="fastproof_disable", action="store_true", default=None, help="turn reporting off")
    g.addoption("--fastproof-finish", dest="fastproof_finish", choices=("auto", "always", "never"), help="finish the launch at the end")
    g.addoption("--fastproof-flush-timeout", dest="fastproof_flush_timeout", type=float, help="seconds to wait for uploads at the end")
    g.addoption("--fastproof-secret-env", dest="fastproof_secret_env", action="append", default=[], help="env var name or glob whose value is masked")
    g.addoption("--fastproof-secret-pattern", dest="fastproof_secret_pattern", action="append", default=[], help="regex to mask")
    parser.addini("fastproof_url", "FastProof server URL")
    parser.addini("fastproof_project", "FastProof project key")
    parser.addini("fastproof_secret_env", "env var names or globs whose values are masked", type="linelist", default=[])
    parser.addini("fastproof_secret_patterns", "regular expressions to mask", type="linelist", default=[])
    parser.addini("fastproof_log_level", "lowest level of streamed log records (default INFO)", default="")


def _resolve(config: pytest.Config) -> Config:
    attrs = {}
    for a in config.getoption("fastproof_attrs", default=[]) or []:
        if "=" in a:
            k, v = a.split("=", 1)
            attrs[k] = v
    opts = {
        "url": _opt(config, "fastproof_url", "fastproof_url"),
        "project": _opt(config, "fastproof_project", "fastproof_project"),
        "launch": _opt(config, "fastproof_launch"),
        "launch_name": _opt(config, "fastproof_launch_name"),
        "environment": _opt(config, "fastproof_environment"),
        "attributes": attrs,
        "spool_dir": _opt(config, "fastproof_spool_dir"),
        "offline": _opt(config, "fastproof_offline"),
        "disable": _opt(config, "fastproof_disable"),
        "finish_launch": _opt(config, "fastproof_finish"),
        "flush_timeout": _opt(config, "fastproof_flush_timeout"),
        "secret_env": list(config.getoption("fastproof_secret_env", default=[]) or []) + list(config.getini("fastproof_secret_env") or []),
        "secret_patterns": list(config.getoption("fastproof_secret_pattern", default=[]) or []) + list(config.getini("fastproof_secret_patterns") or []),
        "log_level": config.getini("fastproof_log_level") or None,
    }
    return resolve_config(opts, cwd=str(config.rootpath))


class XdistHooks:
    def __init__(self, plugin: "FastProofPlugin") -> None:
        self.plugin = plugin

    def pytest_configure_node(self, node: Any) -> None:
        p = self.plugin
        if p.cfg is not None and p.cfg.enabled:
            node.workerinput["fastproof"] = {"launch": p.cfg.launch, "runId": p.run_id}


class FastProofPlugin:
    def __init__(self, config: pytest.Config) -> None:
        self.config = config
        self.cfg: Optional[Config] = None
        self.run_id = secrets.token_hex(6)
        self.client: Optional[Client] = None
        self.worker_client: Optional[Client] = None
        self.executor: Optional[Executor] = None
        self.reporter: Optional[Reporter] = None
        self.plan: Optional[TestPlan] = None
        self.staging = ""
        self.message = ""

    def setup(self) -> None:
        global _executor
        config = self.config
        is_worker = hasattr(config, "workerinput")
        controller = not is_worker and config.getoption("dist", default="no") not in (None, "no")
        try:
            plan = load_test_plan(_opt(config, "fastproof_testplan"))
            self.plan = TestPlan(plan) if plan is not None else None
        except (OSError, ValueError) as e:
            self.message = f"test plan ignored, running everything: {e}"
        self.cfg = _resolve(config)
        cfg = self.cfg
        if is_worker:
            wi = config.workerinput.get("fastproof")
            if not wi:
                return
            cfg.launch = wi["launch"]
            cfg.own_launch = False
            cfg.finish_launch = "never"
            cfg.running_results = False
            self.run_id = wi["runId"]
        if not cfg.enabled:
            if cfg.reason not in ("disabled", "FASTPROOF_URL is not set") and not is_worker:
                self.message = f"reporting is off: {cfg.reason}"
            return
        self.staging = os.path.join(cfg.spool_dir, ".staging", self.run_id)
        if is_worker:
            self.worker_client = Client(cfg, role="worker")
            self.worker_client.start(flush_old=False)
            self.executor = Executor(cfg, self.run_id, self.worker_client, self.staging)
        else:
            self.client = Client(cfg)
            self.client.start()
            self.reporter = Reporter(cfg, self.run_id, self.client)
            if not controller:
                self.executor = Executor(cfg, self.run_id, self.client, self.staging)
            if config.pluginmanager.hasplugin("xdist"):
                config.pluginmanager.register(XdistHooks(self), "fastproof-xdist")
        if self.executor is not None:
            self.executor.install_handler()
            _executor = self.executor
        if not is_worker and config.getoption("allure_report_dir", default=None):
            self.message = (
                "results are sent to FastProof live and also written to --alluredir: do not upload that directory"
                " with `fastproof upload` as well (that makes a second launch), or set FASTPROOF_ENABLED=false"
            )

    def _guard(self, fn, *args) -> None:
        try:
            fn(*args)
        except Exception as e:
            c = self.client or self.worker_client
            if c is not None:
                c.warn_once(f"hook:{type(e).__name__}", f"reporter error (ignored): {e}")

    @pytest.hookimpl(trylast=True)
    def pytest_collection_modifyitems(self, session: pytest.Session, config: pytest.Config, items: List[pytest.Item]) -> None:
        if self.plan is None:
            return
        keep, drop = [], []
        for item in items:
            ids = [str(m.args[0]) for m in item.iter_markers("fastproof_id") if m.args]
            ids += [
                str(m.args[0]) for m in item.iter_markers("allure_label")
                if m.args and m.kwargs.get("label_type") in ("as_id", "ALLURE_ID")
            ]
            (keep if self.plan.matches(item.nodeid, ids) else drop).append(item)
        if drop:
            config.hook.pytest_deselected(items=drop)
            items[:] = keep

    def pytest_collection_finish(self, session: pytest.Session) -> None:
        if self.reporter is not None:
            self.reporter.items = {it.nodeid: it for it in session.items}

    @pytest.hookimpl(tryfirst=True)
    def pytest_runtest_logstart(self, nodeid: str, location: Any) -> None:
        if self.executor is not None:
            self._guard(self.executor.begin, nodeid)
        if self.reporter is not None:
            self._guard(self.reporter.logstart, nodeid, location)

    @pytest.hookimpl(hookwrapper=True)
    def pytest_runtest_makereport(self, item: pytest.Item, call: Any):
        outcome = yield
        if self.executor is None or self.executor.current is None:
            return
        try:
            rep = outcome.get_result()
            rep.fastproof = self.executor.meta(item, self.executor.current, rep.when)
        except Exception as e:
            c = self.client or self.worker_client
            if c is not None:
                c.warn_once("meta", f"reporter error (ignored): {e}")

    def pytest_runtest_logreport(self, report: pytest.TestReport) -> None:
        if self.reporter is not None and report.when is not None:
            self._guard(self.reporter.logreport, report)

    @pytest.hookimpl(trylast=True)
    def pytest_runtest_logfinish(self, nodeid: str, location: Any) -> None:
        if self.reporter is not None:
            self._guard(self.reporter.logfinish, nodeid)
        if self.executor is not None:
            self.executor.current = None

    @pytest.hookimpl(trylast=True)
    def pytest_sessionfinish(self, session: pytest.Session, exitstatus: int) -> None:
        global _executor
        if self.executor is not None:
            self.executor.remove_handler()
            _executor = None
        if self.worker_client is not None:
            self.worker_client.cfg.flush_timeout = min(self.worker_client.cfg.flush_timeout, 10.0)
            self.worker_client.close("finished")
        if self.reporter is not None and self.client is not None:
            self._guard(self.reporter.finish_open)
            interrupted = exitstatus in (pytest.ExitCode.INTERRUPTED, pytest.ExitCode.INTERNAL_ERROR)
            self.reporter.summary = self.client.close("interrupted" if interrupted else "finished")
            if self.staging:
                shutil.rmtree(self.staging, ignore_errors=True)

    def pytest_terminal_summary(self, terminalreporter: Any) -> None:
        lines = []
        if self.message:
            lines.append(f"[fastproof] {self.message}")
        s = self.reporter.summary if self.reporter is not None else None
        if s:
            where = f"launch #{s['launchNumber']}" if s.get("launchNumber") is not None else "launch"
            if s["spooled"]:
                reason = f" ({s['offlineReason']})" if s.get("offlineReason") else ""
                lines.append(f"[fastproof] {s['sent']} results sent, {s['spooled']} saved to {s['spoolDir']}{reason}")
                lines.append(
                    f"[fastproof] the next run sends them, or now: python -m pytest_fastproof flush --spool {self.cfg.spool_dir}"
                    " (with FASTPROOF_URL, FASTPROOF_TOKEN and FASTPROOF_PROJECT set)"
                )
            elif not self.cfg.quiet:
                lines.append(f"[fastproof] {where}: {s['sent']} results, {s['attachments']} attachments sent")
            if s["lost"]:
                lines.append(f"[fastproof] {s['lost']} results could not be reported")
        for ln in lines:
            terminalreporter.write_line(ln)


def pytest_configure(config: pytest.Config) -> None:
    config.addinivalue_line("markers", "fastproof_id(id): explicit FastProof test case id (also used by test plans)")
    config.addinivalue_line("markers", "fastproof_label(name, value): a FastProof label")
    config.addinivalue_line("markers", "fastproof_link(url, name=None, type='link'): a link on the FastProof result")
    plugin = FastProofPlugin(config)
    try:
        plugin.setup()
    except Exception as e:
        plugin.message = f"reporting is off: {e}"
    config.pluginmanager.register(plugin, "fastproof-plugin")


@pytest.fixture
def fastproof_trace() -> TraceContext:
    """Trace context of the current attempt (``traceparent``, ``baggage``, ``headers()``, ``env()``)."""
    att = _current()
    return att.trace if att is not None else TraceContext.new("", "", 0)


def current_attempt() -> Optional[Attempt]:
    return _current()


def current_executor() -> Optional[Executor]:
    return _executor
