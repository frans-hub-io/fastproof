"""A minimal PostgreSQL simple-query client (trust authentication only) for creating test databases."""

import socket
import struct
from urllib.parse import unquote, urlparse


def _msg(kind: bytes, payload: bytes) -> bytes:
    return kind + struct.pack("!I", len(payload) + 4) + payload


def _read(sock: socket.socket, n: int) -> bytes:
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise ConnectionError("connection closed")
        buf += chunk
    return buf


def _until_ready(sock: socket.socket) -> None:
    error = None
    while True:
        kind = _read(sock, 1)
        (length,) = struct.unpack("!I", _read(sock, 4))
        body = _read(sock, length - 4)
        if kind == b"R":
            (code,) = struct.unpack("!I", body[:4])
            if code != 0:
                raise RuntimeError("only trust authentication is supported")
        elif kind == b"E":
            fields = dict((f[:1], f[1:].decode()) for f in body.split(b"\x00") if f)
            error = fields.get(b"M", "error")
        elif kind == b"Z":
            if error:
                raise RuntimeError(error)
            return


def execute(url: str, sql: str) -> None:
    u = urlparse(url)
    user = unquote(u.username or "postgres")
    db = (u.path or "/postgres").lstrip("/") or "postgres"
    with socket.create_connection((u.hostname or "127.0.0.1", u.port or 5432), timeout=30) as s:
        params = b"user\x00" + user.encode() + b"\x00database\x00" + db.encode() + b"\x00\x00"
        startup = struct.pack("!I", 196608) + params
        s.sendall(struct.pack("!I", len(startup) + 4) + startup)
        _until_ready(s)
        s.sendall(_msg(b"Q", sql.encode() + b"\x00"))
        _until_ready(s)
        s.sendall(_msg(b"X", b""))
