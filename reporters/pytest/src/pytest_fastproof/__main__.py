"""``python -m pytest_fastproof flush|list``: send or show what earlier runs left in the spool."""

from __future__ import annotations

import argparse
import sys
import time
from typing import List, Optional

from .api import Api
from .config import resolve_config
from .spool import flush_spool, list_sessions


def main(argv: Optional[List[str]] = None) -> int:
    p = argparse.ArgumentParser(prog="pytest-fastproof", description="FastProof spool tools")
    sub = p.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("flush", help="send spooled results (FASTPROOF_URL, FASTPROOF_TOKEN, FASTPROOF_PROJECT)")
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
    if not cfg.url or not cfg.project:
        print("FASTPROOF_URL and FASTPROOF_PROJECT are required", file=sys.stderr)
        return 2
    api = Api(cfg.url, cfg.token, cfg.project, cfg.request_timeout)
    r = flush_spool(api, cfg.spool_dir, deadline=time.time() + a.timeout, project=cfg.project)
    print(f"sent {r['results']} results from {r['sessions']} sessions; {r['remaining']} left, {r['failed']} rejected")
    for e in r["errors"][:5]:
        print(f"  {e}", file=sys.stderr)
    return 1 if r["remaining"] else 0


if __name__ == "__main__":
    sys.exit(main())
