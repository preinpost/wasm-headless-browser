#!/usr/bin/env python3
"""80 downloads per load and thousands of inherited theme variables."""
import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

lock = threading.Lock()
counts = {"pages": 0, "assets": 0}


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        path = urlsplit(self.path).path
        status = 200
        if path == "/":
            with lock:
                counts["pages"] += 1
                generation = counts["pages"]
            links = "".join(f'<link rel="stylesheet" href="/style/{n}?g={generation}">' for n in range(40))
            scripts = "".join(f'<script src="/script/{n}?g={generation}"></script>' for n in range(40))
            body = (f'<!doctype html><title>Fetch Stress</title>{links}'
                    f'<script>globalThis.__loaded = 0;</script>{scripts}<h1>Fetch Stress</h1>'
                    + '<span>theme</span>' * 800).encode()
            kind = "text/html"
        elif path.startswith("/style/") or path.startswith("/script/"):
            with lock:
                counts["assets"] += 1
            script = path.startswith("/script/")
            if script:
                body = b"globalThis.__loaded += 1;"
            elif path == "/style/0":
                variables = ";".join(f"--theme-token-{n}: value-{n}" for n in range(1600))
                body = (":root {" + variables + "} span::before { content: none }").encode()
            else:
                body = b"h1 { color: #123456; }"
            kind = "application/javascript" if script else "text/css"
        elif path == "/metrics":
            with lock:
                body = json.dumps(counts).encode()
            kind = "application/json"
        else:
            status, body, kind = 404, b"not found", "text/plain"
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
