"""Tiny pass-through proxy that logs request bodies (never headers) to a JSONL file.

Usage: python logging_proxy.py <port> <log.jsonl> [upstream_base]
Point kelir at it with KELIR_BASE_URL=http://127.0.0.1:<port>/api/v1
The Authorization header is forwarded to the upstream and never written anywhere.
"""
import http.server, json, sys, threading, urllib.request

PORT = int(sys.argv[1]); LOG = sys.argv[2]
UP = (sys.argv[3] if len(sys.argv) > 3 else "https://openrouter.ai").rstrip("/")
lock = threading.Lock()

class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"
    def do_GET(self):
        # steer6 review: forward GETs (the model list) so kelir sees the provider's capabilities.
        hdrs = {k.lower(): v for k, v in self.headers.items()}
        req = urllib.request.Request(UP + self.path, method="GET",
              headers={k: v for k, v in hdrs.items() if k in ("authorization", "accept", "user-agent")})
        try:
            r = urllib.request.urlopen(req, timeout=60); body = r.read(); code = r.status
        except urllib.error.HTTPError as e:
            code = e.code; body = e.read()
        self.send_response(code); self.send_header("content-type", "application/json"); self.end_headers(); self.wfile.write(body)
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", 0)))
        hdrs = {k.lower(): v for k, v in self.headers.items()}
        with lock, open(LOG, "a", encoding="utf-8") as f:
            f.write(json.dumps({"path": self.path, "user_agent": hdrs.get("user-agent"),
                                "x_opencode_session": hdrs.get("x-opencode-session"),
                                "body": json.loads(body)}) + "\n")
        req = urllib.request.Request(UP + self.path, data=body, method="POST",
              headers={k: v for k, v in hdrs.items() if k in ("content-type", "authorization", "accept", "user-agent")})
        try:
            r = urllib.request.urlopen(req, timeout=300)
        except urllib.error.HTTPError as e:
            self.send_response(e.code); self.end_headers(); self.wfile.write(e.read()); return
        self.send_response(r.status)
        self.send_header("content-type", r.headers.get("content-type", "text/event-stream"))
        self.end_headers()
        while True:
            chunk = r.read1(4096)
            if not chunk: break
            self.wfile.write(chunk); self.wfile.flush()
    def log_message(self, *a): pass

http.server.ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
