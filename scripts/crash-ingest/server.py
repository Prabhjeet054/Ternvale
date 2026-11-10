#!/usr/bin/env python3
"""Opt-in crash ingest for Ternvale.

POST /v1/crash  JSON body (max 2 MiB). Stores under --dir as crash-<id>.json.
GET  /health    {"ok": true, "retention_days": N}
GET  /v1/crash/<id>  stored JSON (local debugging only; disable in production).

Retention: files older than --retention-days (default 90) are deleted on each
request and when --purge-now is passed. TLS: pass --cert and --key, or terminate
TLS at a reverse proxy and bind --bind 127.0.0.1.

No telemetry. The app never POSTs unless the user opted in and confirmed the
preview. Do not log request bodies (they may contain host logs after redaction).
"""
from __future__ import annotations

import argparse
import json
import ssl
import sys
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse

MAX_BODY = 2 * 1024 * 1024
MAGIC_VERSION = 1


def purge(directory: Path, retention_days: int, now: float | None = None) -> int:
    cutoff = (now if now is not None else time.time()) - retention_days * 86400
    removed = 0
    if not directory.is_dir():
        return 0
    for path in directory.glob("crash-*.json"):
        try:
            if path.stat().st_mtime < cutoff:
                path.unlink()
                removed += 1
        except OSError:
            continue
    return removed


class CrashHandler(BaseHTTPRequestHandler):
    directory: Path
    retention_days: int
    allow_get: bool

    def log_message(self, fmt: str, *args) -> None:
        sys.stderr.write("crash-ingest: " + (fmt % args) + "\n")

    def _json(self, code: int, body: dict) -> None:
        raw = json.dumps(body).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self) -> None:  # noqa: N802
        path = urlparse(self.path).path
        if path == "/health":
            self._json(
                200,
                {
                    "ok": True,
                    "retention_days": self.retention_days,
                    "format_version": MAGIC_VERSION,
                },
            )
            return
        if path.startswith("/v1/crash/") and self.allow_get:
            ident = path.rsplit("/", 1)[-1]
            if not ident.isalnum():
                self._json(400, {"error": "bad id"})
                return
            target = self.directory / f"crash-{ident}.json"
            if not target.is_file():
                self._json(404, {"error": "not found"})
                return
            self._json(200, json.loads(target.read_text(encoding="utf-8")))
            return
        self._json(404, {"error": "not found"})

    def do_POST(self) -> None:  # noqa: N802
        if urlparse(self.path).path != "/v1/crash":
            self._json(404, {"error": "not found"})
            return
        purge(self.directory, self.retention_days)
        length = int(self.headers.get("Content-Length", "0"))
        if length <= 0 or length > MAX_BODY:
            self._json(413, {"error": "body too large or empty"})
            return
        raw = self.rfile.read(length)
        try:
            payload = json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            self._json(400, {"error": "invalid json"})
            return
        if not isinstance(payload, dict):
            self._json(400, {"error": "object required"})
            return
        ident = uuid.uuid4().hex[:16]
        record = {
            "magic": "TVCR",
            "version": MAGIC_VERSION,
            "id": ident,
            "received_unix": int(time.time()),
            "payload": payload,
        }
        self.directory.mkdir(parents=True, exist_ok=True)
        dest = self.directory / f"crash-{ident}.json"
        dest.write_text(json.dumps(record, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        self.log_message("stored id=%s bytes=%s", ident, length)
        self._json(201, {"ok": True, "id": ident, "retention_days": self.retention_days})


def make_server(
    bind: str,
    port: int,
    directory: Path,
    retention_days: int,
    allow_get: bool,
) -> ThreadingHTTPServer:
    CrashHandler.directory = directory
    CrashHandler.retention_days = retention_days
    CrashHandler.allow_get = allow_get
    httpd = ThreadingHTTPServer((bind, port), CrashHandler)
    return httpd


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--bind", default="127.0.0.1")
    p.add_argument("--port", type=int, default=8444)
    p.add_argument("--dir", type=Path, default=Path("crash-store"))
    p.add_argument("--retention-days", type=int, default=90)
    p.add_argument("--cert", type=Path, default=None)
    p.add_argument("--key", type=Path, default=None)
    p.add_argument("--allow-get", action="store_true")
    p.add_argument("--purge-now", action="store_true")
    args = p.parse_args(argv)
    if args.purge_now:
        n = purge(args.dir, args.retention_days)
        print(f"purged {n} files")
        return 0
    httpd = make_server(args.bind, args.port, args.dir, args.retention_days, args.allow_get)
    if args.cert and args.key:
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(str(args.cert), str(args.key))
        httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)
        scheme = "https"
    else:
        scheme = "http"
        print("crash-ingest: no --cert/--key; serving plain HTTP (put TLS in front)", file=sys.stderr)
    print(f"crash-ingest: {scheme}://{args.bind}:{args.port}/  retention={args.retention_days}d")
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        return 0
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
