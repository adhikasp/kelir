"""Pass-through logging proxy for the M5-B review: raw request body text + response usage.

Usage: python m5b_proxy.py <port> <log.jsonl> [upstream_base] [fault]
Each line: {"n", "stream", "raw" (the request body, exact text), "status", "usage" (from the answer, if any)}.
The Authorization header is forwarded upstream and NEVER written. fault: "500first" answers the first
request with a 500 (retry test).
"""
import http.server, json, os, sys, threading, time, urllib.request

PORT = int(sys.argv[1]); LOG = sys.argv[2]
UP = (sys.argv[3] if len(sys.argv) > 3 and sys.argv[3] else "https://openrouter.ai").rstrip("/")
FAULT = sys.argv[4] if len(sys.argv) > 4 else ""
lock = threading.Lock(); count = {"n": 0}

def usage_of(text):
    best = None
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("data:"):
            line = line[5:].strip()
        if not line.startswith("{"):
            continue
        try:
            v = json.loads(line)
        except Exception:
            continue
        if isinstance(v, dict) and v.get("usage"):
            best = v["usage"]
    if best is None:
        try:
            best = json.loads(text).get("usage")
        except Exception:
            pass
    return best

class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", 0)))
        raw = body.decode("utf-8")
        with lock:
            count["n"] += 1; n = count["n"]
        hdrs = {k.lower(): v for k, v in self.headers.items()}
        parsed = json.loads(raw)
        entry = {"n": n, "path": self.path, "stream": bool(parsed.get("stream")), "raw": raw}
        if entry["stream"] and os.path.exists(LOG + ".hold"):
            with open(LOG + ".arrived", "w", encoding="utf-8", newline="") as f: f.write(raw)
            while os.path.exists(LOG + ".hold"): time.sleep(0.02)
        if FAULT == "500first" and n == 1:
            entry["status"] = 500
            with lock, open(LOG, "a", encoding="utf-8") as f: f.write(json.dumps(entry) + "\n")
            self.send_response(500); self.end_headers(); self.wfile.write(b'{"error":"injected"}'); return
        req = urllib.request.Request(UP + self.path, data=body, method="POST",
              headers={k: v for k, v in hdrs.items() if k in ("content-type", "authorization", "accept", "user-agent")})
        try:
            r = urllib.request.urlopen(req, timeout=300)
            status = r.status; ctype = r.headers.get("content-type", "text/event-stream")
        except urllib.error.HTTPError as e:
            data = e.read(); entry["status"] = e.code
            with lock, open(LOG, "a", encoding="utf-8") as f: f.write(json.dumps(entry) + "\n")
            self.send_response(e.code); self.end_headers(); self.wfile.write(data); return
        self.send_response(status); self.send_header("content-type", ctype); self.end_headers()
        buf = b""
        while True:
            chunk = r.read1(4096)
            if not chunk: break
            buf += chunk
            try:
                self.wfile.write(chunk); self.wfile.flush()
            except Exception:
                break
        entry["status"] = status; entry["usage"] = usage_of(buf.decode("utf-8", "replace"))
        entry["answer"] = buf.decode("utf-8", "replace")[:400] if not entry["stream"] else None
        with lock, open(LOG, "a", encoding="utf-8") as f: f.write(json.dumps(entry) + "\n")
    def log_message(self, *a): pass

http.server.ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
