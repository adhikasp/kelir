"""Pass-through proxy that records when each completion request starts and ends, and how many
were in flight, to prove a concurrency limit from the outside.

Usage: python steer5_overlap_proxy.py <port> <log.jsonl> [upstream_base]
Point kelir at it with KELIR_BASE_URL=http://127.0.0.1:<port>/api/v1
Headers and bodies are never written; the Authorization header is only forwarded upstream.
"""
import http.server, json, sys, threading, time, urllib.request

PORT = int(sys.argv[1]); LOG = sys.argv[2]
UP = (sys.argv[3] if len(sys.argv) > 3 else "https://openrouter.ai").rstrip("/")
lock = threading.Lock(); st = {"inflight": 0, "peak": 0, "n": 0}

def note(**kw):
    with lock, open(LOG, "a", encoding="utf-8") as f:
        f.write(json.dumps(dict(t=time.time(), **kw)) + "\n")

class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", 0)))
        hdrs = {k.lower(): v for k, v in self.headers.items()}
        with lock:
            st["n"] += 1; n = st["n"]; st["inflight"] += 1; st["peak"] = max(st["peak"], st["inflight"])
            now_inflight = st["inflight"]
        note(ev="start", n=n, inflight=now_inflight)
        try:
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
        finally:
            with lock:
                st["inflight"] -= 1; left = st["inflight"]
            note(ev="end", n=n, inflight_after=left, peak=st["peak"])
    def log_message(self, *a): pass

http.server.ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
