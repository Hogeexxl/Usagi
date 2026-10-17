#!/usr/bin/env python3
import argparse
import datetime as dt
import http.server
import json
import os
import pathlib
import socketserver
import sys
import time
from urllib.parse import parse_qs, urlparse


fixture_dir = pathlib.Path(os.environ["USAGI_MAGPIE_FIXTURE_DIR"])
fixture_dir.mkdir(parents=True, exist_ok=True)
now = dt.datetime.now().astimezone().replace(microsecond=0)
now_text = now.isoformat()
day = now.date().isoformat()
key = os.environ["MAGPIE_WEB_KEY"]
print(
    f"fixture startup: python={sys.version!r} executable={sys.executable!r} "
    f"now={now_text} TZ={os.environ.get('TZ')!r} "
    f"block_daily={os.environ.get('USAGI_MAGPIE_BLOCK_DAILY')!r}",
    file=sys.stderr,
    flush=True,
)

row = {
    "t": now_text,
    "agent": "codex",
    "agentName": "Codex",
    "provider": "openai",
    "providerName": "OpenAI",
    "model": "gpt-6-luna",
    "in": 10,
    "out": 5,
    "ms": 1000,
    "ttft_ms": 100,
    "status": 200,
    "session": "smoke-session",
    "cost": 0,
    "priced": False,
    "source": "magpie-http",
}
point = {
    "label": day,
    "time": now_text,
    "calls": 1,
    "input": 10,
    "output": 5,
}


class LoopbackHTTPServer(http.server.ThreadingHTTPServer):
    def server_bind(self):
        print(f"fixture before bind: address={self.server_address!r}", file=sys.stderr, flush=True)
        # This numeric loopback fixture does not need HTTPServer's reverse DNS lookup.
        socketserver.TCPServer.server_bind(self)
        self.server_name, self.server_port = self.server_address[:2]
        print(f"fixture after bind: address={self.server_address!r}", file=sys.stderr, flush=True)


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, _format, *_args):
        return

    def reply(self, status, value=None, headers=None):
        parsed = urlparse(self.path)
        print(
            f"fixture response: path={parsed.path} status={status}",
            file=sys.stderr,
            flush=True,
        )
        body = b"" if value is None else json.dumps(value).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Length", str(len(body)))
        if value is not None:
            self.send_header("Content-Type", "application/json")
        for name, header_value in (headers or {}).items():
            self.send_header(name, header_value)
        self.end_headers()
        if body:
            self.wfile.write(body)

    def do_GET(self):
        parsed = urlparse(self.path)
        params = parse_qs(parsed.query)
        print(
            f"fixture request: path={parsed.path} "
            f"period={params.get('period')} limit={params.get('limit')} "
            f"cookie_matches={self.headers.get('Cookie') == f'magpie-web={key}'}",
            file=sys.stderr,
            flush=True,
        )
        if parsed.path == "/" and params.get("k") == [key]:
            self.reply(200, {}, {"Set-Cookie": f"magpie-web={key}; Path=/; HttpOnly"})
            return
        if self.headers.get("Cookie") != f"magpie-web={key}":
            self.reply(401, {"error": "unauthorized"})
            return
        if parsed.path == "/api/sessions/progress":
            self.reply(200, {"indexing": False, "files": 1, "done": 1, "bytes": 0, "read": 0})
            return
        if parsed.path == "/api/sessions":
            self.reply(200, {
                "sessions": [{
                    "agent": "codex",
                    "id": "smoke-session",
                    "cwd": "/fixture/Usagi",
                    "title": "Temporary runtime smoke session",
                    "start": now_text,
                    "last": now_text,
                    "name": "Codex",
                    "path": "/fixture/Usagi/session.jsonl",
                }],
                "terminal": False,
                "dirs": [],
            })
            return
        if parsed.path == "/api/usage/quotas":
            self.reply(200, [{"provider": "claude", "name": "Claude", "windows": []}])
            return
        if parsed.path == "/api/providers":
            self.reply(200, {"providers": [{"id": "openai", "catalog": "openai"}], "fileError": ""})
            return
        if parsed.path == "/api/usage":
            self.reply(200, {
                "period": params.get("period", ["all"])[0],
                "bucket": "day",
                "series": [point],
                "calls": 1,
                "input": 10,
                "output": 5,
            })
            return
        if parsed.path == "/api/usage/requests":
            period = params.get("period", [""])[0]
            limit = int(params.get("limit", ["0"])[0])
            if os.environ.get("USAGI_MAGPIE_BLOCK_DAILY") == "1" and limit >= 500 and period == f"{day}..{day}":
                (fixture_dir / "sync_waiting").touch()
                release = fixture_dir / "release_sync"
                deadline = time.monotonic() + 60
                while not release.exists() and time.monotonic() < deadline:
                    time.sleep(0.025)
                if not release.exists():
                    self.reply(504, {"error": "fixture daily sync release timed out"})
                    return
            rows = [row]
            offset = int(params.get("offset", ["0"])[0])
            selected = rows[offset:offset + limit] if limit else rows[offset:]
            self.reply(200, {
                "period": period,
                "rows": selected,
                "offset": offset,
                "total": len(rows),
                "bucket": "day",
                "series": [point],
                "providers": [{"id": "openai", "name": "OpenAI", "icon": ""}],
                "agents": [{"id": "codex", "name": "Codex", "icon": ""}],
                "purposes": [],
                "accounts": [],
                "through": {},
                "direct": {},
                "calls": 1,
                "input": 10,
                "output": 5,
            })
            return
        self.reply(404, {"error": "not found"})


parser = argparse.ArgumentParser()
parser.add_argument("--addr", required=True)
arguments, _ = parser.parse_known_args()
host, port = arguments.addr.rsplit(":", 1)
server = LoopbackHTTPServer((host, int(port)), Handler)
(fixture_dir / "magpie.pid").write_text(str(os.getpid()), encoding="utf-8")
server.serve_forever()
