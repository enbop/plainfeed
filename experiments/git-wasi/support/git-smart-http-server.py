#!/usr/bin/env python3
"""Minimal local Git smart-HTTP fixture for the WASI push experiment."""

from __future__ import annotations

import argparse
import subprocess
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse


def packet_line(payload: bytes) -> bytes:
    return f"{len(payload) + 4:04x}".encode("ascii") + payload


class Handler(BaseHTTPRequestHandler):
    repository: Path

    def do_GET(self) -> None:
        request = urlparse(self.path)
        service = parse_qs(request.query).get("service", [None])[0]
        if request.path != "/repo.git/info/refs" or service != "git-receive-pack":
            self.send_error(404)
            return

        advertised = subprocess.run(
            ["git", "receive-pack", "--stateless-rpc", "--advertise-refs", self.repository],
            check=True,
            capture_output=True,
        ).stdout
        body = packet_line(b"# service=git-receive-pack\n") + b"0000" + advertised
        self.send_response(200)
        self.send_header("Content-Type", "application/x-git-receive-pack-advertisement")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self) -> None:
        if urlparse(self.path).path != "/repo.git/git-receive-pack":
            self.send_error(404)
            return

        length = int(self.headers.get("Content-Length", "0"))
        request_body = self.rfile.read(length)
        result = subprocess.run(
            ["git", "receive-pack", "--stateless-rpc", self.repository],
            input=request_body,
            capture_output=True,
        )
        if result.returncode != 0:
            self.send_error(500, result.stderr.decode("utf-8", errors="replace"))
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/x-git-receive-pack-result")
        self.send_header("Content-Length", str(len(result.stdout)))
        self.end_headers()
        self.wfile.write(result.stdout)

    def log_message(self, message: str, *args: object) -> None:
        print(f"smart-http: {message % args}", flush=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("repository", type=Path)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", default=18080, type=int)
    args = parser.parse_args()
    Handler.repository = args.repository.resolve()
    server = ThreadingHTTPServer((args.host, args.port), Handler)
    print(f"serving {Handler.repository} at http://{args.host}:{args.port}/repo.git", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
