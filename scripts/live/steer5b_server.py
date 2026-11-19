"""Canned OpenAI-style server for the STEER-5 round-2 live drive (no keys, no network).

Usage: python steer5b_server.py <port> <log.jsonl> <script>
<script> is a ';'-separated list of replies, consumed one per completion request, then "ok" forever:
  429:<raw Retry-After value>   a 429 with that header verbatim ("-" = no header)
  <code>:echo                   that status with a body echoing the Authorization header (a leaky gateway)
  503 / 400                     plain errors
  ok                            a text answer ("all done")
  tool                          a tool call head (bash) then the connection is cut (a started tool call)
  cut                           half a text answer, then the connection closes
Each request is logged as {"t","n","reply","model","auth":bool,"inflight"}: never headers or bodies.
"""
import http.server, json, sys, threading, time

PORT = int(sys.argv[1]); LOG = sys.argv[2]
SCRIPT = [s for s in sys.argv[3].split(";") if s]
lock = threading.Lock(); state = {"n": 0, "inflight": 0}

def sse(chunks):
    return "".join(f"data: {json.dumps(c)}\n\n" for c in chunks) + "data: [DONE]\n\n"

class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"
    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get("content-length", 0)))
        try: model = json.loads(raw).get("model")
        except Exception: model = None
        auth = self.headers.get("Authorization", "")
        with lock:
            state["n"] += 1; n = state["n"]
            state["inflight"] += 1
            reply = SCRIPT[n - 1] if n <= len(SCRIPT) else "ok"
            with open(LOG, "a", encoding="utf-8") as f:
                f.write(json.dumps({"t": time.time(), "n": n, "reply": reply, "model": model, "auth": bool(auth), "inflight": state["inflight"]}) + "\n")
        try:
            kind, _, arg = reply.partition(":")
            if kind.isdigit():
                body = (f"echo: Authorization={auth} x-api-key={self.headers.get('x-api-key','')}" if arg == "echo" else "canned failure").encode()
                self.send_response(int(kind)); self.send_header("content-length", str(len(body)))
                if kind == "429" and arg not in ("", "-", "echo"):
                    self.send_header("Retry-After", arg)
                self.end_headers(); self.wfile.write(body)
            elif reply == "cut":
                self.send_response(200); self.send_header("content-type", "text/event-stream"); self.end_headers()
                self.wfile.write(b'data: {"choices":[{"delta":{"content":"half an ans"}}]}\n\n'); self.wfile.flush()
            elif reply == "tool":
                self.send_response(200); self.send_header("content-type", "text/event-stream"); self.end_headers()
                c = {"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": "bash", "arguments": "{\"command\":\"echo TOOLRAN > tool_ran.txt\"}"}}]}}]}
                self.wfile.write(f"data: {json.dumps(c)}\n\n".encode()); self.wfile.flush()
            else:
                chunks = [{"choices": [{"delta": {"content": "all done"}}]},
                          {"choices": [{"delta": {}, "finish_reason": "stop"}]},
                          {"choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 5, "cost": 0}}]
                body = sse(chunks).encode()
                self.send_response(200); self.send_header("content-type", "text/event-stream")
                self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)
        finally:
            with lock: state["inflight"] -= 1
    def log_message(self, *a): pass

http.server.ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
