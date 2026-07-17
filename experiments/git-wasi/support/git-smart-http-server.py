#!/usr/bin/env python3
"""Minimal local Git smart-HTTP fixture for WASI fetch and push tests."""

from __future__ import annotations

import argparse
import os
import subprocess
import tempfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse


def packet_line(payload: bytes) -> bytes:
    return f"{len(payload) + 4:04x}".encode("ascii") + payload


class Handler(BaseHTTPRequestHandler):
    repository: Path
    advances_remaining: int = 0
    advance_sequence: int = 0

    def do_GET(self) -> None:
        request = urlparse(self.path)
        service = parse_qs(request.query).get("service", [None])[0]
        if request.path != "/repo.git/info/refs" or service not in {
            "git-upload-pack",
            "git-receive-pack",
        }:
            self.send_error(404)
            return

        if (
            service == "git-receive-pack"
            and type(self).advances_remaining > 0
        ):
            type(self).advances_remaining -= 1
            type(self).advance_sequence += 1
            self.advance_with_content_commit()

        advertised = subprocess.run(
            [
                "git",
                service.removeprefix("git-"),
                "--stateless-rpc",
                "--advertise-refs",
                self.repository,
            ],
            check=True,
            capture_output=True,
        ).stdout
        body = packet_line(f"# service={service}\n".encode()) + b"0000" + advertised
        self.send_response(200)
        self.send_header("Content-Type", f"application/x-{service}-advertisement")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self) -> None:
        service = urlparse(self.path).path.removeprefix("/repo.git/")
        if service not in {"git-upload-pack", "git-receive-pack"}:
            self.send_error(404)
            return

        length = int(self.headers.get("Content-Length", "0"))
        request_body = self.rfile.read(length)
        result = subprocess.run(
            ["git", service.removeprefix("git-"), "--stateless-rpc", self.repository],
            input=request_body,
            capture_output=True,
        )
        if result.returncode != 0:
            self.send_error(500, result.stderr.decode("utf-8", errors="replace"))
            return
        self.send_response(200)
        self.send_header("Content-Type", f"application/x-{service}-result")
        self.send_header("Content-Length", str(len(result.stdout)))
        self.end_headers()
        self.wfile.write(result.stdout)

    def advance_with_content_commit(self) -> None:
        with tempfile.TemporaryDirectory(prefix="plainfeed-race-") as directory:
            worktree = Path(directory) / "worktree"
            subprocess.run(
                ["git", "clone", "--quiet", str(self.repository), str(worktree)],
                check=True,
            )
            suffix = "" if type(self).advance_sequence == 1 else f"-{type(self).advance_sequence}"
            entry_id = f"race-content{suffix}"
            path = worktree / "content" / "2026" / "07" / f"{entry_id}.md"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(
                """+++
format = "plainfeed.entry/v1"
id = "{entry_id}"
title = "Content committed during state publication"
published = "2026-07-17T04:30:00Z"
summary = "This entry verifies that a competing content commit is preserved."
channels = ["technology"]
source = {{ name = "Plainfeed test", url = "https://example.com/{entry_id}" }}
+++

The state publisher must rebuild its candidate on top of this commit.
""".format(entry_id=entry_id),
                encoding="utf-8",
            )
            environment = os.environ.copy()
            environment.update(
                {
                    "GIT_AUTHOR_NAME": "Plainfeed race fixture",
                    "GIT_AUTHOR_EMAIL": "race@plainfeed.invalid",
                    "GIT_COMMITTER_NAME": "Plainfeed race fixture",
                    "GIT_COMMITTER_EMAIL": "race@plainfeed.invalid",
                }
            )
            subprocess.run(["git", "-C", worktree, "add", str(path)], check=True)
            subprocess.run(
                ["git", "-C", worktree, "commit", "--quiet", "-m", "test: race state publication"],
                check=True,
                env=environment,
            )
            subprocess.run(
                ["git", "-C", worktree, "push", "--quiet", "origin", "main"],
                check=True,
            )

    def log_message(self, message: str, *args: object) -> None:
        print(f"smart-http: {message % args}", flush=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("repository", type=Path)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", default=18080, type=int)
    parser.add_argument("--advance-on-first-push", action="store_true")
    parser.add_argument("--advance-pushes", default=0, type=int)
    args = parser.parse_args()
    Handler.repository = args.repository.resolve()
    Handler.advances_remaining = max(
        args.advance_pushes,
        1 if args.advance_on_first_push else 0,
    )
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
