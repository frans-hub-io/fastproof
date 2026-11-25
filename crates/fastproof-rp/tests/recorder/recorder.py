#!/usr/bin/env python3
"""Minimal ReportPortal-compatible recorder for capturing real agent traffic.

It answers just enough of the RP reporting API for the official agents to run and writes
every exchange as one NDJSON line. Authorization headers are never written; multipart file
parts are stored as size and sha256 only.

Usage: recorder.py --out raw.ndjson [--port 8089] [--host 127.0.0.1] [--fault METHOD:REGEX:STATUS:COUNT]

--fault answers the first COUNT matching requests with STATUS and an ErrorRS body, so retry
behaviour of the agents is recorded too. The request is recorded with "injected": true.
"""
import argparse
import gzip
import hashlib
import json
import re
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

KEPT_HEADERS = {"content-type", "accept", "user-agent", "content-encoding", "transfer-encoding", "expect"}
LOCK = threading.Lock()
STATE = {"launch_no": 0, "item_no": 0, "launches": {}, "items": {}}
ARGS = None

LAUNCH_FINISH = re.compile(r"^launch/([^/]+)/finish$")
LAUNCH_BY_UUID = re.compile(r"^launch/uuid/([^/]+)$")
LAUNCH_UPDATE = re.compile(r"^launch/([^/]+)/update$")
ITEM_BY_UUID = re.compile(r"^item/uuid/([^/]+)$")
ITEM_UPDATE = re.compile(r"^item/([^/]+)/update$")
ITEM_CHILD = re.compile(r"^item/([^/]+)$")


def read_body(handler):
    if handler.headers.get("Transfer-Encoding", "").lower() == "chunked":
        out = b""
        while True:
            size = int(handler.rfile.readline().strip().split(b";")[0] or b"0", 16)
            if size == 0:
                handler.rfile.readline()
                return out
            out += handler.rfile.read(size)
            handler.rfile.readline()
    length = int(handler.headers.get("Content-Length") or 0)
    return handler.rfile.read(length) if length else b""


def parse_multipart(body, content_type):
    m = re.search(r'boundary="?([^";]+)"?', content_type)
    if not m:
        return [{"error": "no boundary"}]
    sep = b"--" + m.group(1).encode()
    parts = []
    for chunk in body.split(sep)[1:]:
        if chunk.startswith(b"--"):
            break
        chunk = chunk[2:] if chunk.startswith(b"\r\n") else chunk
        head, _, data = chunk.partition(b"\r\n\r\n")
        data = data[:-2] if data.endswith(b"\r\n") else data
        headers = {}
        for line in head.decode("utf-8", "replace").split("\r\n"):
            k, _, v = line.partition(":")
            headers[k.strip().lower()] = v.strip()
        disp = headers.get("content-disposition", "")
        name = re.search(r'\bname="([^"]*)"', disp)
        fname = re.search(r'filename="([^"]*)"', disp)
        part = {"name": name.group(1) if name else None, "contentType": headers.get("content-type")}
        if fname:
            part["filename"] = fname.group(1)
        if fname or (part["contentType"] or "").split(";")[0].strip() not in ("application/json", "text/plain", ""):
            part["size"] = len(data)
            part["sha256"] = hashlib.sha256(data).hexdigest()
        else:
            try:
                part["json"] = json.loads(data)
            except ValueError:
                part["text"] = data.decode("utf-8", "replace")
        parts.append(part)
    return parts


def error(code, message):
    return 404 if code == 40 else 400, {"errorCode": code, "message": message}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def do_GET(self):
        self.handle_any()

    def do_POST(self):
        self.handle_any()

    def do_PUT(self):
        self.handle_any()

    def do_DELETE(self):
        self.handle_any()

    def handle_any(self):
        raw = read_body(self)
        ctype = self.headers.get("Content-Type", "")
        if self.headers.get("Content-Encoding", "").lower() == "gzip":
            raw = gzip.decompress(raw)
        split = urlsplit(self.path)
        path = split.path
        entry = {
            "t": time.time(),
            "method": self.command,
            "path": path,
            "query": parse_qs(split.query),
            "headers": {k.lower(): v for k, v in self.headers.items() if k.lower() in KEPT_HEADERS},
            "authorization": "present" if self.headers.get("Authorization") else "absent",
        }
        if raw:
            if ctype.startswith("multipart/"):
                entry["multipart"] = parse_multipart(raw, ctype)
            else:
                try:
                    entry["body"] = json.loads(raw)
                except ValueError:
                    entry["bodyText"] = raw[:2000].decode("utf-8", "replace")
        fault = self.take_fault(path)
        if fault:
            status, payload = fault, {"errorCode": 5000, "message": "injected fault"}
            entry["injected"] = True
        else:
            status, payload = self.route(path, entry)
        entry["response"] = {"status": status, "body": payload}
        with LOCK:
            with open(ARGS.out, "a") as f:
                f.write(json.dumps(entry, sort_keys=True) + "\n")
        data = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def take_fault(self, path):
        with LOCK:
            for f in ARGS.faults:
                if f["method"] == self.command and f["regex"].search(path) and f["left"] > 0:
                    f["left"] -= 1
                    return f["status"]
        return None

    def route(self, path, entry):
        m = re.match(r"^/rp/api/(info)$", path) or re.match(r"^/rp/api/(v1|v2)/([^/]+)/(.*)$", path)
        if not m:
            return 501, {"errorCode": 5000, "message": "not supported by FastProof"}
        if m.group(1) == "info":
            return 200, {"build": {"version": "5.14.0", "name": "FastProof RP compatibility"}}
        version, project, rest = m.group(1), m.group(2), m.group(3).rstrip("/")
        body = entry.get("body") or {}
        method = self.command
        with LOCK:
            if rest == "launch" and method == "POST":
                STATE["launch_no"] += 1
                lid = body.get("uuid") or str(uuid.uuid4())
                STATE["launches"][lid] = STATE["launch_no"]
                return 201, {"id": lid, "number": STATE["launch_no"]}
            if rest == "launch" and method == "GET":
                content = [{"id": n, "uuid": u, "name": "recorded", "number": n, "status": "IN_PROGRESS"}
                           for u, n in STATE["launches"].items()]
                return 200, {"content": content, "page": {"number": 1, "size": max(len(content), 1),
                                                          "totalElements": len(content), "totalPages": 1}}
            if rest == "launch/merge" and method == "POST":
                return 200, {"id": 1, "uuid": str(uuid.uuid4()), "name": body.get("name", "merged")}
            mm = LAUNCH_FINISH.match(rest)
            if mm and method == "PUT":
                n = STATE["launches"].get(mm.group(1), 1)
                return 200, {"id": mm.group(1), "number": n,
                             "link": f"https://fastproof.example.com/rp/ui/#{project}/launches/all/{n}"}
            mm = LAUNCH_BY_UUID.match(rest)
            if mm and method == "GET":
                n = STATE["launches"].get(mm.group(1), 1)
                return 200, {"id": n, "uuid": mm.group(1), "name": "recorded", "number": n, "status": "IN_PROGRESS"}
            mm = LAUNCH_UPDATE.match(rest)
            if mm and method == "PUT":
                return 200, {"message": f"Launch with ID = '{mm.group(1)}' successfully updated."}
            if rest.startswith("item") and method == "POST":
                if rest not in ("item",) and not ITEM_CHILD.match(rest):
                    return 404, {"errorCode": 40, "message": "unknown item path"}
                STATE["item_no"] += 1
                iid = body.get("uuid") or str(uuid.uuid4())
                STATE["items"][iid] = STATE["item_no"]
                if version == "v2":
                    return 201, {"id": iid}
                return 201, {"id": iid, "uniqueId": "auto:" + hashlib.md5(iid.encode()).hexdigest()}
            mm = ITEM_BY_UUID.match(rest)
            if mm and method == "GET":
                n = STATE["items"].get(mm.group(1), 1)
                return 200, {"id": n, "uuid": mm.group(1), "name": "recorded", "status": "IN_PROGRESS"}
            mm = ITEM_UPDATE.match(rest)
            if mm and method == "PUT":
                return 200, {"message": f"TestItem with ID = '{mm.group(1)}' successfully updated."}
            mm = ITEM_CHILD.match(rest)
            if mm and method == "PUT":
                return 200, {"message": f"TestItem with ID = '{mm.group(1)}' successfully finished."}
            if rest == "log/entry" and method == "POST":
                return 201, {"id": str(uuid.uuid4())}
            if rest == "log" and method == "POST":
                if "multipart" in entry:
                    jp = next((p for p in entry["multipart"] if p.get("name") == "json_request_part"), None)
                    items = (jp or {}).get("json") or []
                    return 201, {"responses": [{"id": it.get("uuid") or str(uuid.uuid4())} for it in items]}
                return 201, {"id": str(uuid.uuid4())}
            if rest == "settings" and method == "GET":
                return 200, {"project": 1, "subTypes": {
                    "PRODUCT_BUG": [{"id": 1, "locator": "pb001", "typeRef": "PRODUCT_BUG", "longName": "Product Bug",
                                     "shortName": "PB", "color": "#f7d63e"}],
                    "AUTOMATION_BUG": [{"id": 2, "locator": "ab001", "typeRef": "AUTOMATION_BUG",
                                        "longName": "Automation Bug", "shortName": "AB", "color": "#f5a623"}],
                    "SYSTEM_ISSUE": [{"id": 3, "locator": "si001", "typeRef": "SYSTEM_ISSUE",
                                      "longName": "System Issue", "shortName": "SI", "color": "#0274d1"}],
                    "NO_DEFECT": [{"id": 4, "locator": "nd001", "typeRef": "NO_DEFECT",
                                   "longName": "No Defect", "shortName": "ND", "color": "#777777"}],
                    "TO_INVESTIGATE": [{"id": 5, "locator": "ti001", "typeRef": "TO_INVESTIGATE",
                                        "longName": "To Investigate", "shortName": "TI", "color": "#ffb743"}]}}
        return 501, {"errorCode": 5000, "message": "not supported by FastProof"}


def main():
    global ARGS
    p = argparse.ArgumentParser()
    p.add_argument("--out", required=True)
    p.add_argument("--port", type=int, default=8089)
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--fault", action="append", default=[])
    ARGS = p.parse_args()
    ARGS.faults = []
    for spec in ARGS.fault:
        method, regex, status, count = spec.split(":")
        ARGS.faults.append({"method": method, "regex": re.compile(regex), "status": int(status), "left": int(count)})
    ThreadingHTTPServer.daemon_threads = True
    ThreadingHTTPServer((ARGS.host, ARGS.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
