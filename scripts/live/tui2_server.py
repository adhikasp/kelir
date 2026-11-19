#!/usr/bin/env python3
"""Scripted streaming OpenAI-style server for the tui2 review drives (no keys, no network).

usage: tui2_server.py <port> <scenario.json> <log.jsonl>
scenario = list of turns; request k answers with turn k (the last turn repeats afterwards).
a turn = list of events:
  {"text": "...", "chunk": 7, "delay": 0.02}   stream text in `chunk`-char pieces, `delay` s apart
  {"tool": {"id":"c1","name":"bash","args":{"command":"..."}}}
  {"sleep": 1.5}                                 pause the stream
  {"status": 429, "body": "..."}                 answer with an HTTP error instead
Every POST body is appended to <log.jsonl> as {"path":..,"body":..}.
"""
import http.server, json, sys, time

PORT = int(sys.argv[1])
SCEN = json.load(open(sys.argv[2], encoding="utf-8"))
LOG = sys.argv[3]
n = {"i": 0}


def sse(c):
    return ("data: " + json.dumps(c) + "\n\n").encode()


class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def do_GET(self):
        body = json.dumps({"data": []}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get("content-length", 0)))
        try:
            body = json.loads(raw)
        except Exception:
            body = {"raw": raw.decode("utf-8", "replace")}
        with open(LOG, "a", encoding="utf-8") as f:
            f.write(json.dumps({"path": self.path, "body": body}) + "\n")
        n["i"] += 1
        turn = SCEN[min(n["i"], len(SCEN)) - 1] if SCEN else [{"text": "ok"}]
        for ev in turn:
            if "status" in ev:
                b = ev.get("body", "{}").encode()
                self.send_response(ev["status"])
                self.send_header("content-length", str(len(b)))
                self.end_headers()
                self.wfile.write(b)
                return
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        w = self.wfile
        tools = 0
        fin = "stop"
        out = 0
        try:
            for ev in turn:
                if "sleep" in ev:
                    time.sleep(ev["sleep"])
                elif "text" in ev:
                    t = ev["text"]
                    k = ev.get("chunk", 12)
                    for j in range(0, len(t), k):
                        piece = t[j:j + k]
                        out += max(1, len(piece) // 4)
                        w.write(sse({"choices": [{"delta": {"content": piece}}]}))
                        w.flush()
                        time.sleep(ev.get("delay", 0.02))
                elif "tool" in ev:
                    c = ev["tool"]
                    fin = "tool_calls"
                    w.write(sse({"choices": [{"delta": {"tool_calls": [{"index": tools, "id": c["id"], "type": "function",
                                "function": {"name": c["name"], "arguments": json.dumps(c["args"])}}]}}]}))
                    w.flush()
                    tools += 1
            w.write(sse({"choices": [{"delta": {}, "finish_reason": fin}]}))
            w.write(sse({"choices": [], "usage": {"prompt_tokens": 1200 * n["i"], "completion_tokens": out,
                         "prompt_tokens_details": {"cached_tokens": 1000 * (n["i"] - 1)}, "cost": 0.0012}}))
            w.write(b"data: [DONE]\n\n")
            w.flush()
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass

    def log_message(self, *a):
        pass


http.server.ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
