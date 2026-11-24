"""A tiny OpenAI-compatible server that misbehaves on `GET /models` on purpose.

For the STEER-1 review (provider model metadata). It answers
`POST /chat/completions` with one short assistant turn, so `kelir run --ask` ends, and
answers `GET /models` according to the mode. Every request is appended to the log
file as one JSON line: the wall-clock time it arrived, the method, the path, and
the request header *names* only. No header value is ever written, so a run with a
real key in the environment leaves no secret behind.

usage: python models_probe.py <port> <mode> [logfile]
modes:
  ok                 the OpenRouter shape, four models
  go                 the OpenCode Go shape (limit.context + capabilities)
  unauth             HTTP 401 (metadata), completion still fine
  forbidden          HTTP 403
  server             HTTP 500
  malformed          HTTP 200 with a body that is not JSON
  slow:<ms>          HTTP 200 after <ms> milliseconds
  hang               accept the request and never answer (until killed)
  huge:<n>           HTTP 200 with <n> models (500 by default when n is omitted)
  completion401      401 for the completion too (metadata is ok)

The log is opened in append mode and flushed per line; timestamps are seconds
since the epoch as a float, so a caller can subtract them.
"""

import http.server
import json
import sys
import time

PORT = int(sys.argv[1])
MODE = sys.argv[2]
LOG = sys.argv[3] if len(sys.argv) > 3 else ""

MODELS = {
    "data": [
        {
            "id": "deepseek/deepseek-v4.1-flash-1017",
            "context_length": 1048576,
            "supported_parameters": ["tools", "parallel_tool_calls", "reasoning"],
        },
        {
            "id": "test/model",
            "context_length": 262144,
            "supported_parameters": ["tools"],
        },
        {
            "id": "small/model",
            "context_length": 8192,
            "supported_parameters": ["tools"],
        },
    ]
}

GO_MODELS = {
    "object": "list",
    "data": [
        {
            "id": "deepseek-v4.1-flash",
            "limit": {"context": 1000000, "output": 384000},
            "capabilities": {"tools": {"supported": True}},
        }
    ],
}


def log(entry):
    entry["t"] = time.time()
    line = json.dumps(entry, sort_keys=True)
    if LOG:
        with open(LOG, "a", encoding="utf-8") as handle:
            handle.write(line + "\n")
    print(line, flush=True)


def sse(chunks):
    return "".join(f"data: {json.dumps(c)}\n\n" for c in chunks) + "data: [DONE]\n\n"


class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def log_message(self, *args):
        pass

    def _note(self):
        return {
            "path": self.path.split("?")[0],
            "method": self.command,
            # Names only: a value could be a key.
            "headers": sorted(self.headers.keys()),
            "authorized": self.headers.get("authorization") is not None,
        }

    def do_GET(self):
        note = self._note()
        if not self.path.split("?")[0].rstrip("/").endswith("/models"):
            log({**note, "answer": "404"})
            self.send_response(404)
            self.end_headers()
            return
        mode = MODE
        if mode == "hang":
            log({**note, "answer": "hang"})
            time.sleep(3600)
            return
        if mode.startswith("slow:"):
            delay = int(mode.split(":", 1)[1]) / 1000.0
            # Log the arrival before the wait: the timestamp must say when the
            # request came in, not when the answer went out.
            log({**note, "answer": "slow", "delay_ms": int(delay * 1000)})
            time.sleep(delay)
            mode = "ok"
        if mode.startswith("huge"):
            count = int(mode.split(":", 1)[1]) if ":" in mode else 500
            body = json.dumps(
                {
                    "data": [
                        {
                            "id": f"vendor{i}/model-{i}",
                            "context_length": 4096 + i,
                            "supported_parameters": ["tools"],
                        }
                        for i in range(count)
                    ]
                }
            ).encode()
            log({**note, "answer": "huge", "count": count})
            self._send(200, "application/json", body)
            return
        if mode == "unauth":
            log({**note, "answer": "401"})
            self._send(401, "text/plain", b"no key for you")
            return
        if mode == "forbidden":
            log({**note, "answer": "403"})
            self._send(403, "text/plain", b"forbidden")
            return
        if mode == "server":
            log({**note, "answer": "500"})
            self._send(500, "text/plain", b"the model list is down")
            return
        if mode == "malformed":
            log({**note, "answer": "malformed"})
            self._send(200, "application/json", b'{"data": [ {"id": ')
            return
        payload = GO_MODELS if mode == "go" else MODELS
        log({**note, "answer": "ok"})
        self._send(200, "application/json", json.dumps(payload).encode())

    def do_POST(self):
        length = int(self.headers.get("content-length", 0))
        body = self.rfile.read(length)
        log({**self._note(), "answer": "completion", "bytes": len(body)})
        if MODE == "completion401":
            self._send(401, "text/plain", b"bad key")
            return
        chunks = [
            {"choices": [{"delta": {"content": "probe: done"}}]},
            {"choices": [{"delta": {}, "finish_reason": "stop"}]},
            {
                "choices": [],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 3,
                    "prompt_tokens_details": {"cached_tokens": 0},
                    "cost": 0,
                },
            },
        ]
        self._send(200, "text/event-stream", sse(chunks).encode())

    def _send(self, status, kind, body):
        self.send_response(status)
        self.send_header("content-type", kind)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


http.server.ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
