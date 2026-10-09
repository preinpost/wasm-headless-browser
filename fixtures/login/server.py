"""A login site for verify-login.sh, standing in for the reference site.

The page is an empty shell that app.js renders, the login is a fetch() POST,
the session is an HttpOnly cookie, and the signed-in page reads it back from
an API. That is the shape of the real thing, without its credentials.

    python3 fixtures/login/server.py 8140

The only account is kim / hunter2. Nothing here logs request bodies.
"""

import json
import secrets
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).parent
SESSIONS = {}

SHELL = b"""<!doctype html><html lang="en"><head><meta charset="utf-8">
<title>Fixture Login</title></head>
<body><div id="app">Loading</div><script src="/app.js" defer></script></body></html>"""


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def send(self, status, body, content_type="application/json", headers=()):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        for name, value in headers:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)

    def session_user(self):
        for part in self.headers.get("Cookie", "").split(";"):
            name, _, value = part.strip().partition("=")
            if name == "sid" and value in SESSIONS:
                return SESSIONS[value]
        return None

    def do_GET(self):
        path = self.path.split("?")[0]
        if path in ("/", "/home"):
            self.send(200, SHELL, "text/html; charset=utf-8")
        elif path == "/app.js":
            self.send(200, (HERE / "app.js").read_bytes(), "text/javascript")
        elif path == "/api/me":
            user = self.session_user()
            if user:
                self.send(200, json.dumps({"name": user}).encode())
            else:
                self.send(401, b'{"error":"not signed in"}')
        else:
            self.send(404, b"not found", "text/plain")

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        if self.path != "/api/login":
            self.send(404, b"not found", "text/plain")
            return
        try:
            form = json.loads(body)
        except ValueError:
            form = {}
        if form.get("user") == "kim" and form.get("password") == "hunter2":
            sid = secrets.token_hex(16)
            SESSIONS[sid] = "kim"
            self.send(200, b'{"ok":true}', headers=[("Set-Cookie", f"sid={sid}; Path=/; HttpOnly")])
        else:
            self.send(401, b'{"error":"Invalid credentials"}')


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8140
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
