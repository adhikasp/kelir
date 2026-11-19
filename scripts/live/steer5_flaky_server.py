"""Flaky OpenAI-style SSE server for the STEER-5 live drive (no network, no keys).

Usage: python steer5_flaky_server.py <port> <log.jsonl> <script>
<script> is a comma list of replies, consumed one per completion request, then "ok" forever:
  429:<retry-after>   a 429 with that Retry-After header (use "-" for none)
  503                 a 503
  ok                  a good text answer ("all done")
  cut                 an SSE head and half a body, then the connection closes
Every request is logged as {"t": epoch_seconds, "n": k, "reply": "...", "inflight": m}; headers and
bodies are never written.
"""
import http.server, json, sys, threading, time

PORT = int(sys.argv[1]); LOG = sys.argv[2]
SCRIPT = [s for s in sys.argv[3].split(",") if s]
lock = threading.Lock(); state = {"n": 0, "inflight": 0, "peak": 0}

def sse(chunks):
    return "".join(f"data: {json.dumps(c)}\n\n" for c in chunks) + "data: [DONE]\n\n"

class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"
    def do_POST(self):
        self.rfile.read(int(self.headers.get("content-length", 0)))
        with lock:
            state["n"] += 1; n = state["n"]
            state["inflight"] += 1; state["peak"] = max(state["peak"], state["inflight"])
            reply = SCRIPT[n - 1] if n <= len(SCRIPT) else "ok"
            with open(LOG, "a", encoding="utf-8") as f:
                f.write(json.dumps({"t": time.time(), "n": n, "reply": reply, "inflight": state["inflight"]}) + "\n")
        try:
            if reply.startswith("429") or reply.startswith("503"):
                code = int(reply[:3]); body = b"slow down (canned)"
                self.send_response(code); self.send_header("content-length", str(len(body)))
                if ":" in reply and reply.split(":", 1)[1] != "-":
                    self.send_header("Retry-After", reply.split(":", 1)[1])
                self.end_headers(); self.wfile.write(body)
            elif reply == "cut":
                self.send_response(200); self.send_header("content-type", "text/event-stream"); self.end_headers()
                self.wfile.write(b'data: {"choices":[{"delta":{"content":"half an ans"}}]}\n\n'); self.wfile.flush()
            else:
                chunks = [{"choices": [{"delta": {"content": "all done"}}]},
                          {"choices": [{"delta": {}, "finish_reason": "stop"}]},
                          {"choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 5, "cost": 0}}]
                body = sse(chunks).encode()
                self.send_response(200); self.send_header("content-type", "text/event-stream")
                self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)
        finally:
            with lock:
                state["inflight"] -= 1
    def log_message(self, *a): pass

http.server.ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
