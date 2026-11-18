"""Canned OpenAI-style SSE server for break-it drives (no network, no keys).

Usage: python canned_server.py <port> '<bash command>'
Request 1 -> one bash tool call running <command>; any later request -> "all done".
Or pass (or "@file" holding) a JSON list of turns instead of a command, each turn a list of
{"id","name","args"} tool calls: request k answers with turn k, then "all done".
"""
import http.server, json, sys

PORT = int(sys.argv[1]); CMD = sys.argv[2]
if CMD.startswith("@"):
    CMD = open(CMD[1:], encoding="utf-8").read()
TURNS = json.loads(CMD) if CMD.lstrip().startswith("[") else [[{"id": "call_1", "name": "bash", "args": {"command": CMD}}]]
n = {"i": 0}

def sse(chunks):
    return "".join(f"data: {json.dumps(c)}\n\n" for c in chunks) + "data: [DONE]\n\n"

class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"
    def do_POST(self):
        self.rfile.read(int(self.headers.get("content-length", 0)))
        n["i"] += 1
        if n["i"] <= len(TURNS):
            calls = [{"index": k, "id": c["id"], "type": "function",
                      "function": {"name": c["name"], "arguments": json.dumps(c["args"])}}
                     for k, c in enumerate(TURNS[n["i"] - 1])]
            chunks = [{"choices": [{"delta": {"tool_calls": calls}}]},
                      {"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}]
        else:
            chunks = [{"choices": [{"delta": {"content": "all done"}}]},
                      {"choices": [{"delta": {}, "finish_reason": "stop"}]}]
        chunks.append({"choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 5, "cost": 0}})
        body = sse(chunks).encode()
        self.send_response(200); self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def log_message(self, *a): pass

http.server.ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
