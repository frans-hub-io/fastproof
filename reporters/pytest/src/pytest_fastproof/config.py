"""Configuration from command-line options, ini values and ``FASTPROOF_*`` environment variables."""

from __future__ import annotations

import os
import re
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any, Dict, List, Mapping, Optional

from .ci import detect_ci
from .mask import DEFAULT_SECRET_ENV

PROJECT_KEY = re.compile(r"^[a-z0-9][a-z0-9-]{1,62}$")


def _flag(v: Optional[str]) -> Optional[bool]:
    if v is None or not v.strip():
        return None
    return v.strip().lower() not in ("0", "false", "no", "off")


def _list(v: Optional[str]) -> List[str]:
    return [s.strip() for s in (v or "").split(",") if s.strip()]


def _num(v: Any, default: float) -> float:
    try:
        n = float(v)
    except (TypeError, ValueError):
        return default
    return n if n >= 0 else default


@dataclass
class Config:
    enabled: bool
    reason: str
    url: str
    token: str
    project: str
    offline: bool
    launch: Dict[str, Any]
    own_launch: bool
    spool_dir: str
    spool_max_bytes: int = 200 * 1024 * 1024
    flush_timeout: float = 30.0
    request_timeout: float = 15.0
    batch_interval: float = 0.2
    running_results: bool = True
    live_logs: bool = True
    log_level: int = 20
    finish_launch: str = "auto"
    secret_env: List[str] = field(default_factory=list)
    secret_values: List[str] = field(default_factory=list)
    secret_patterns: List[str] = field(default_factory=list)
    env: Mapping[str, str] = field(default_factory=dict)
    max_attachment_bytes: int = 512 * 1024 * 1024
    #: ``auto``: attachments from ``direct_upload_min_bytes`` up go straight to the server's S3 store through
    #: pre-signed URLs when the server offers it; ``off``: always through the server.
    direct_uploads: str = "auto"
    direct_upload_min_bytes: int = 1024 * 1024
    quiet: bool = False


def resolve_config(opts: Optional[Mapping[str, Any]] = None, env: Optional[Mapping[str, str]] = None, cwd: Optional[str] = None) -> Config:
    """``opts`` holds already-merged command-line and ini values (``None`` when unset)."""
    o = dict(opts or {})
    env = os.environ if env is None else env
    cwd = cwd or os.getcwd()

    def pick(key: str, var: str) -> Optional[str]:
        v = o.get(key)
        if v is not None and str(v).strip() != "":
            return str(v)
        e = env.get(var)
        return e if e is not None and e.strip() != "" else None

    token = env.get("FASTPROOF_TOKEN") or ""
    if not token and env.get("FASTPROOF_TOKEN_FILE"):
        try:
            with open(env["FASTPROOF_TOKEN_FILE"], encoding="utf-8") as f:
                token = f.read().strip()
        except OSError:
            token = ""
    url = (pick("url", "FASTPROOF_URL") or "").strip()
    project = (pick("project", "FASTPROOF_PROJECT") or "").strip()
    ci = detect_ci(env)
    explicit = (pick("launch", "FASTPROOF_LAUNCH") or "").strip()
    external = explicit or (ci or {}).get("externalId") or f"local-{uuid.uuid4()}"
    stamp = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M")
    name = pick("launch_name", "FASTPROOF_LAUNCH_NAME") or (ci or {}).get("name") or explicit or f"{project or 'tests'} {stamp}"
    launch: Dict[str, Any] = {"externalId": external, "name": name}
    attrs = o.get("attributes") or {}
    if attrs:
        launch["attributes"] = [{"key": k, "value": str(v)} for k, v in attrs.items()]
    if ci and ci.get("ci"):
        launch["ci"] = ci["ci"]
    if ci and ci.get("vcs"):
        launch["vcs"] = ci["vcs"]
    environment = pick("environment", "FASTPROOF_ENVIRONMENT")
    if environment:
        launch["environment"] = {"key": environment.strip(), "values": {}}
    if o.get("debug"):
        launch["debug"] = True

    secret_env = list(o.get("secret_env") or []) + _list(env.get("FASTPROOF_SECRET_ENV"))
    patterns = list(o.get("secret_patterns") or []) + _list(env.get("FASTPROOF_SECRET_PATTERNS"))

    enabled = _flag(env.get("FASTPROOF_ENABLED")) is not False and not o.get("disable")
    offline = bool(o.get("offline")) or bool(_flag(env.get("FASTPROOF_OFFLINE")))
    reason = "" if enabled else "disabled"
    if enabled and not url and not offline:
        enabled, reason = False, "FASTPROOF_URL is not set"
    elif enabled and not PROJECT_KEY.match(project):
        enabled, reason = False, "FASTPROOF_PROJECT is missing or not a valid project key"
    spool = pick("spool_dir", "FASTPROOF_SPOOL_DIR") or ".fastproof/spool"
    flush_timeout = o.get("flush_timeout")
    if flush_timeout is None and env.get("FASTPROOF_FLUSH_TIMEOUT_MS"):
        flush_timeout = _num(env["FASTPROOF_FLUSH_TIMEOUT_MS"], 30000) / 1000
    flush_timeout = _num(flush_timeout, 30.0)
    level = str(o.get("log_level") or env.get("FASTPROOF_LOG_LEVEL") or "INFO").upper()
    levels = {"TRACE": 5, "DEBUG": 10, "INFO": 20, "WARNING": 30, "WARN": 30, "ERROR": 40, "CRITICAL": 50}
    return Config(
        enabled=enabled,
        reason=reason,
        url=url,
        token=token,
        project=project,
        offline=offline,
        launch=launch,
        own_launch=not explicit and not (ci or {}).get("externalId"),
        spool_dir=os.path.abspath(os.path.join(cwd, spool)),
        spool_max_bytes=int(_num(env.get("FASTPROOF_SPOOL_MAX_BYTES"), 200 * 1024 * 1024)),
        flush_timeout=flush_timeout,
        live_logs=_flag(env.get("FASTPROOF_LIVE_LOGS")) is not False,
        log_level=levels.get(level, 20),
        finish_launch=str(o.get("finish_launch") or "auto"),
        secret_env=list(DEFAULT_SECRET_ENV) + secret_env,
        secret_values=[token] if token else [],
        secret_patterns=patterns,
        env=env,
        direct_uploads="off" if (env.get("FASTPROOF_DIRECT_UPLOAD_MODE") or "").strip().lower() == "off" else "auto",
        direct_upload_min_bytes=max(1, int(_num(env.get("FASTPROOF_DIRECT_UPLOAD_MIN_BYTES"), 1024 * 1024))),
        quiet=bool(_flag(env.get("FASTPROOF_QUIET"))),
    )
