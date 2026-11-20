"""Scripted OpenAI-style SSE server for the M7 review (no network, no keys).

usage: python m7_mock.py <port> <script.json> [<requests.jsonl>]

script.json maps a TASK KEY to a list of bash commands, one per model step. The task
key is the text of the first `user` message of the request that begins with a key in
the table (prefix match), so a root, its children and a brief each get their own script
by the task text kelir hands them. A step is chosen by the number of assistant messages
already in the request. After the list the model answers "done:<key>". A step may be the
string "@sleep N" (a bash `sleep N`). Request bodies are logged (never headers).
The usage block carries prompt_tokens / cached_tokens so `kelir ls` has numbers.
"""
import http.server, json, sys, threading

PORT = int(sys.argv[1])
SCRIPT = json.load(open(sys.argv[2], encoding="utf-8"))
LOG = sys.argv[3] if len(sys.argv) > 3 else None
lock = threading.Lock()
seen_prefix = {}

def first_user(body):
    for m in body.get("messages", []):
        if m.get("role") == "user":
            c = m.get("content")
            if isinstance(c, list):
                c = "".join(p.get("text", "") for p in c if isinstance(p, dict))
            return c or ""
    return ""

def last_user(body):
    out = ""
    for m in body.get("messages", []):
        if m.get("role") == "user":
            c = m.get("content")
            if isinstance(c, list):
                c = "".join(p.get("text", "") for p in c if isinstance(p, dict))
            out = c or ""
    return out

def sse(chunks):
    return "".join(f"data: {json.dumps(c)}\n\n" for c in chunks) + "data: [DONE]\n\n"

class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"
    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get("content-length", 0)))
        body = json.loads(raw)
        if LOG:
            with lock, open(LOG, "a", encoding="utf-8") as f:
                f.write(json.dumps({"path": self.path, "raw_len": len(raw), "raw": raw.decode("utf-8"), "body": body}) + "\n")
        task = first_user(body)
        # a brief's question is the LAST user message; scripts for it are keyed "BRIEF:<question prefix>"
        key = next((k for k in SCRIPT if not k.startswith("BRIEF:") and task.startswith(k)), None)
        lu = last_user(body)
        bkey = next((k for k in SCRIPT if k.startswith("BRIEF:") and lu.startswith(k[6:])), None)
        if bkey:
            key = bkey
        steps = SCRIPT.get(key, []) if key else []
        n_asst = sum(1 for m in body.get("messages", []) if m.get("role") == "assistant")
        if bkey:
            # count only assistants after the brief question
            idx = max(i for i, m in enumerate(body["messages"]) if m.get("role") == "user")
            n_asst = sum(1 for m in body["messages"][idx:] if m.get("role") == "assistant")
        if n_asst < len(steps):
            cmd = steps[n_asst]
            tool, targs = "bash", {"command": cmd}
            if cmd.startswith("@sleep "):
                targs = {"command": "sleep " + cmd.split()[1]}
            elif cmd.startswith("@tool "):
                _, tool, rest = cmd.split(" ", 2)
                targs = json.loads(rest)
            calls = [{"index": 0, "id": f"call_{n_asst}", "type": "function",
                      "function": {"name": tool, "arguments": json.dumps(targs)}}]
            chunks = [{"choices": [{"delta": {"tool_calls": calls}}]},
                      {"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}]
        else:
            chunks = [{"choices": [{"delta": {"content": "done:" + (key or task)[:60]}}]},
                      {"choices": [{"delta": {}, "finish_reason": "stop"}]}]
        pt = max(100, len(raw) // 4)
        sid = body.get("session_id") or self.headers.get("x-opencode-session") or ""
        prev = seen_prefix.get(sid, 0)
        cached = min(pt, prev)
        seen_prefix[sid] = pt
        chunks.append({"choices": [], "usage": {"prompt_tokens": pt, "completion_tokens": 7, "cost": 0.0001,
                        "prompt_tokens_details": {"cached_tokens": cached}}})
        out = sse(chunks).encode()
        self.send_response(200); self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(out))); self.end_headers(); self.wfile.write(out)
    def log_message(self, *a): pass

http.server.ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
