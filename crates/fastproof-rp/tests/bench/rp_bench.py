#!/usr/bin/env python3
"""Synthetic ReportPortal agent load against /rp (design 72, section 9).

Profiles: js (v2, one request per log line), java (v2, multipart batches of 10 log lines),
python (v2, multipart batches of 20). Each test is one STEP item with STEPS nested steps
and LOGS log lines. Prints latency percentiles per request kind, throughput and the settle
lag (launch finish until every result is out of "running").

Usage: rp_bench.py --url http://127.0.0.1:8080 --project bench --token T --profile js --tests 500 --workers 12
Needs aiohttp.
"""
import argparse
import asyncio
import json
import statistics
import time
import uuid
from datetime import datetime, timezone

import aiohttp

KINDS = ("item_start", "item_finish", "log")


def now():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f+0000")


class Stats:
    def __init__(self):
        self.lat = {k: [] for k in KINDS}
        self.codes = {}

    def add(self, kind, started, code):
        self.lat[kind].append((time.perf_counter() - started) * 1000)
        self.codes[code] = self.codes.get(code, 0) + 1


def pct(values, p):
    if not values:
        return float("nan")
    s = sorted(values)
    return s[min(len(s) - 1, int(len(s) * p / 100))]


async def call(session, stats, kind, method, url, **kw):
    started = time.perf_counter()
    for attempt in range(6):
        async with session.request(method, url, **kw) as r:
            body = await r.read()
            code = r.status
            if code == 503 and attempt < 5:
                stats.add(kind, started, code)
                await asyncio.sleep(1)
                started = time.perf_counter()
                continue
            stats.add(kind, started, code)
            return code, body
    return code, body


def log_entry(launch, item, i, level="INFO"):
    return {"launchUuid": launch, "itemUuid": item, "time": now(), "level": level, "message": f"log line {i} " + "x" * 80}


async def send_logs(session, stats, base, launch, item, args):
    lines = [log_entry(launch, item, i) for i in range(args.logs)]
    if args.profile == "js":
        for line in lines:
            await call(session, stats, "log", "POST", f"{base}/log/entry", json=line)
        return
    size = 10 if args.profile == "java" else 20
    for i in range(0, len(lines), size):
        form = aiohttp.FormData()
        form.add_field("json_request_part", json.dumps(lines[i : i + size]), content_type="application/json")
        await call(session, stats, "log", "POST", f"{base}/log", data=form)


async def one_test(session, stats, base, launch, suite, n, args):
    t = str(uuid.uuid4())
    await call(session, stats, "item_start", "POST", f"{base}/item/{suite}", json={
        "uuid": t, "launchUuid": launch, "name": f"test_{n}", "type": "STEP", "hasStats": True,
        "codeRef": f"bench/test_{n // 100}.py:test_{n}", "testCaseId": f"bench/test_{n // 100}.py:test_{n}",
        "startTime": now(), "attributes": [{"key": "owner", "value": f"team-{n % 5}"}],
    })
    for s in range(args.steps):
        sid = str(uuid.uuid4())
        await call(session, stats, "item_start", "POST", f"{base}/item/{t}", json={
            "uuid": sid, "launchUuid": launch, "name": f"step {s}", "type": "STEP", "hasStats": False, "startTime": now(),
        })
        await call(session, stats, "item_finish", "PUT", f"{base}/item/{sid}", json={
            "launchUuid": launch, "endTime": now(), "status": "PASSED",
        })
    await send_logs(session, stats, base, launch, t, args)
    failed = n % 10 == 0
    if failed:
        await send_logs(session, stats, base, launch, t, argparse.Namespace(**{**vars(args), "logs": 1}))
    await call(session, stats, "item_finish", "PUT", f"{base}/item/{t}", json={
        "launchUuid": launch, "endTime": now(), "status": "FAILED" if failed else "PASSED",
    })


async def main():
    p = argparse.ArgumentParser()
    p.add_argument("--url", required=True)
    p.add_argument("--project", required=True)
    p.add_argument("--token", required=True)
    p.add_argument("--profile", choices=("js", "java", "python"), default="js")
    p.add_argument("--tests", type=int, default=500)
    p.add_argument("--workers", type=int, default=12)
    p.add_argument("--steps", type=int, default=10)
    p.add_argument("--logs", type=int, default=50)
    p.add_argument("--json", action="store_true")
    args = p.parse_args()
    base = f"{args.url}/rp/api/v2/{args.project}"
    stats = Stats()
    headers = {"Authorization": f"Bearer {args.token}"}
    conn = aiohttp.TCPConnector(limit=args.workers * 2)
    async with aiohttp.ClientSession(headers=headers, connector=conn, timeout=aiohttp.ClientTimeout(total=120)) as session:
        launch = str(uuid.uuid4())
        await call(session, stats, "item_start", "POST", f"{base}/launch", json={
            "uuid": launch, "name": f"bench-{args.profile}", "startTime": now(), "mode": "DEFAULT",
        })
        suite = str(uuid.uuid4())
        await call(session, stats, "item_start", "POST", f"{base}/item", json={
            "uuid": suite, "launchUuid": launch, "name": "bench", "type": "SUITE", "startTime": now(),
        })
        queue = asyncio.Queue()
        for n in range(args.tests):
            queue.put_nowait(n)

        async def worker():
            while True:
                try:
                    n = queue.get_nowait()
                except asyncio.QueueEmpty:
                    return
                await one_test(session, stats, base, launch, suite, n, args)

        started = time.perf_counter()
        await asyncio.gather(*(worker() for _ in range(args.workers)))
        load_s = time.perf_counter() - started
        await call(session, stats, "item_finish", "PUT", f"{base}/item/{suite}", json={"launchUuid": launch, "endTime": now()})
        finish_at = time.perf_counter()
        await call(session, stats, "item_finish", "PUT", f"{base}/launch/{launch}/finish", json={"endTime": now()})
        async with session.get(f"{args.url}/api/v1/projects/{args.project}/launches?limit=50") as r:
            items = (await r.json())["items"]
        fp = next(i for i in items if i.get("externalId", "").endswith(launch) or i["id"] == launch)
        deadline = time.perf_counter() + 300
        results = 0
        while time.perf_counter() < deadline:
            async with session.get(f"{args.url}/api/v1/projects/{args.project}/launches/{fp['id']}") as r:
                info = await r.json()
            if info["status"] != "running" and info["counts"]["total"] >= args.tests:
                results = info["counts"]["total"]
                break
            await asyncio.sleep(0.25)
        settle = time.perf_counter() - finish_at
    total = sum(len(v) for v in stats.lat.values())
    out = {
        "profile": args.profile, "tests": args.tests, "workers": args.workers, "requests": total,
        "loadSeconds": round(load_s, 2), "requestsPerSecond": round(total / load_s, 1),
        "testsPerSecond": round(args.tests / load_s, 1), "settleSeconds": round(settle, 2), "results": results,
        "codes": stats.codes,
        "latencyMs": {
            k: {"n": len(v), "p50": round(pct(v, 50), 1), "p95": round(pct(v, 95), 1), "p99": round(pct(v, 99), 1),
                "mean": round(statistics.fmean(v), 1) if v else None}
            for k, v in stats.lat.items()
        },
    }
    print(json.dumps(out, indent=None if args.json else 2))


asyncio.run(main())
