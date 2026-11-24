#!/usr/bin/env python3
"""Turn raw recorder output into committed fixtures.

Usage: sanitize.py RAW.ndjson OUT.ndjson [--replace FROM=TO ...]

Uuids are renumbered in order of first appearance, local paths and the recorder address are
replaced, the wall-clock "t" is replaced by the line number "seq". Timestamps in bodies are
kept: they carry the ordering the server has to cope with.
"""
import json
import re
import socket
import sys

UUID = re.compile(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
TMP_NAME = re.compile(r"(note|rp-recording)\d{6,}")


def main():
    raw, out, *rest = sys.argv[1:]
    repl = [
        ("127.0.0.1:8089", "fastproof.example.com"),
        ("/home/node", "/home/runner"),
        (socket.gethostname(), "runner"),
    ]
    i = 0
    while i < len(rest):
        if rest[i] == "--replace":
            a, b = rest[i + 1].split("=", 1)
            repl.insert(0, (a, b))
            i += 2
        else:
            raise SystemExit("bad argument " + rest[i])
    ids = {}

    def uuid_sub(m):
        key = m.group(0).lower()
        if key not in ids:
            ids[key] = "00000000-0000-4000-8000-%012x" % (len(ids) + 1)
        return ids[key]

    with open(raw) as src, open(out, "w") as dst:
        for seq, line in enumerate(src, 1):
            entry = json.loads(line)
            entry.pop("t", None)
            entry["seq"] = seq
            text = json.dumps(entry, sort_keys=True, ensure_ascii=False)
            for a, b in repl:
                text = text.replace(a, b)
            text = TMP_NAME.sub(lambda m: m.group(1) + "0", text)
            text = UUID.sub(uuid_sub, text)
            dst.write(text + "\n")


if __name__ == "__main__":
    main()
