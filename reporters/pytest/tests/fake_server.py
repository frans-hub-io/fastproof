"""A small in-memory FastProof API for the live path: launches, results (with ``running``), logs, attachments."""

from __future__ import annotations

import hashlib
import json
import re
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Dict, List

ROUTE = re.compile(r"^/api/v1/projects/([^/]+)/launches(?:/([^/]+)/(results|logs|attachments|finish))?$")


class FakeServer:
    def __init__(self, token: str = "", logs: bool = True, running: bool = True, port: int = 0) -> None:
        self.token = token
        self.logs_enabled = logs
        self.running_enabled = running
        self.launches: Dict[str, Dict[str, Any]] = {}
        self.results: Dict[str, Dict[str, Any]] = {}
        self.order: List[str] = []
        self.logs: List[Dict[str, Any]] = []
        self.attachments: Dict[str, Dict[str, Any]] = {}
        self.uploads = 0
        self.lock = threading.Lock()
        fake = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def send(self, status: int, body: Any) -> None:
                data = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def do_POST(self):
                n = int(self.headers.get("Content-Length") or 0)
                body = self.rfile.read(n) if n else b""
                if fake.token and self.headers.get("Authorization") != f"Bearer {fake.token}":
                    return self.send(401, {"code": "unauthorized", "message": "no"})
                m = ROUTE.match(self.path)
                if not m:
                    return self.send(404, {"code": "not_found", "message": "resource not found"})
                with fake.lock:
                    return fake.handle(self, m.group(2), m.group(3), body)

        self.server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def __enter__(self) -> "FakeServer":
        self.thread.start()
        return self

    def __exit__(self, *exc) -> None:
        self.server.shutdown()
        self.server.server_close()

    def handle(self, h, launch_id, what, body: bytes):
        if what is None:
            start = json.loads(body)
            for launch in self.launches.values():
                if launch["externalId"] == start["externalId"]:
                    return h.send(200, {"id": launch["id"], "number": launch["number"]})
            launch = {"id": str(uuid.uuid4()), "number": len(self.launches) + 1, "externalId": start["externalId"], "start": start}
            self.launches[launch["id"]] = launch
            return h.send(201, {"id": launch["id"], "number": launch["number"]})
        launch = self.launches.get(launch_id)
        if launch is None:
            return h.send(404, {"code": "not_found", "message": "launch"})
        if what == "finish":
            launch["finished"] = json.loads(body or b"{}").get("status", "finished")
            return h.send(200, {"id": launch_id})
        if what == "attachments":
            self.uploads += 1
            sha = hashlib.sha256(body).hexdigest()
            a = self.attachments.get(sha)
            if a is None:
                a = {"id": str(uuid.uuid4()), "name": h.headers.get("X-FastProof-Attachment-Name"),
                     "contentType": h.headers.get("Content-Type"), "body": body}
                self.attachments[sha] = a
            return h.send(201, {"id": a["id"], "sha256": sha, "size": len(body)})
        lines = [json.loads(ln) for ln in body.decode().split("\n") if ln.strip()]
        if what == "logs":
            if not self.logs_enabled:
                return h.send(405, {"code": "method_not_allowed", "message": "no"})
            self.logs.extend(lines)
            return h.send(202, {"accepted": len(lines)})
        summary = {"accepted": 0, "duplicates": 0, "rejected": []}
        ids = {a["id"] for a in self.attachments.values()}
        for i, r in enumerate(lines):
            if r["status"] == "running" and not self.running_enabled:
                summary["rejected"].append({"index": i, "message": "invalid result: unknown variant `running`"})
                continue
            missing = [a["id"] for a in r.get("attachments") or [] if a["id"] not in ids]
            if missing:
                summary["rejected"].append({"index": i, "message": f"unknown attachment id {missing[0]}"})
                continue
            known = self.results.get(r["id"])
            if known and known["status"] != "running":
                summary["duplicates"] += 1
                continue
            self.order.append(f"{r['id']}:{r['status']}")
            r["history"] = (known or {}).get("history", []) + [r["status"]]
            self.results[r["id"]] = r
            summary["accepted"] += 1
        return h.send(202, summary)

    def finals(self) -> List[Dict[str, Any]]:
        return [r for r in self.results.values() if r["status"] != "running"]
