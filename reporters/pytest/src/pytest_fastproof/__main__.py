"""``python -m pytest_fastproof flush|list``: send or show what earlier runs left in the spool."""

from __future__ import annotations

import argparse
import sys
import time
from typing import List, Optional

from .api import Api
from .config import resolve_config
from .spool import PROJECT_KEY, flush_spool, list_sessions


def main(argv: Optional[List[str]] = None) -> int:
    p = argparse.ArgumentParser(prog="pytest-fastproof", description="FastProof spool tools")
    sub = p.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser(
        "flush",
        help="send spooled results (FASTPROOF_URL, FASTPROOF_TOKEN; FASTPROOF_PROJECT limits it to one project,"
        " otherwise each session goes to the project it was recorded for)",
    )
    f.add_argument("--spool", help="spool directory (default .fastproof/spool or FASTPROOF_SPOOL_DIR)")
    f.add_argument("--timeout", type=float, default=600.0)
    ls = sub.add_parser("list", help="list spooled sessions")
    ls.add_argument("--spool")
    a = p.parse_args(argv)
    cfg = resolve_config({"spool_dir": a.spool})
    if a.cmd == "list":
        for s in list_sessions(cfg.spool_dir):
            m = s["meta"]
            state = "ended" if m.get("ended") else "interrupted"
            print(f"{s['id']}\t{m.get('project')}\t{m.get('launch', {}).get('externalId')}\t{state}")
        return 0
    if not cfg.url:
        print("FASTPROOF_URL is required", file=sys.stderr)
        return 2
    if cfg.project:
        projects = [cfg.project]
    else:
        projects = sorted({str(s["meta"].get("project")) for s in list_sessions(cfg.spool_dir) if isinstance(s["meta"].get("project"), str) and PROJECT_KEY.fullmatch(s["meta"]["project"])})
    deadline = time.time() + a.timeout
    r = {"sessions": 0, "results": 0, "failed": 0, "remaining": 0, "errors": []}
    for project in projects:
        api = Api(cfg.url, cfg.token, project, cfg.request_timeout)
        one = flush_spool(api, cfg.spool_dir, deadline=deadline, project=project)
        for k in ("sessions", "results", "failed", "remaining"):
            r[k] += one[k]
        r["errors"].extend(one["errors"])
    print(f"sent {r['results']} results from {r['sessions']} sessions; {r['remaining']} left, {r['failed']} rejected")
    for e in r["errors"][:5]:
        print(f"  {e}", file=sys.stderr)
    return 1 if r["remaining"] else 0


if __name__ == "__main__":
    sys.exit(main())
