"""Measure what a fixed scripted session puts on the wire, mock only.

usage: python steer6_bench.py --old <binary> --new <binary>

Both binaries run the same scripted conversation (three bash calls, one
multi-hunk `edit`, one final text) against a recording OpenAI-compatible server
on 127.0.0.1. No provider, no key, no network beyond loopback.

Printed per binary: the byte size of each request body as it arrived, the size
of the system message and of the tool JSON, and the totals. The system and tools
blocks are frozen per session (paid once); the messages array is re-sent every
step.
"""
import argparse
import glob
import http.server
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time

TURNS = [
    [{"id": "c1", "name": "bash", "args": {"command": "printf 'a\\n'"}}],
    [{"id": "c2", "name": "bash", "args": {"command": "sed -n '1,40p' src/prompt.md"}}],
    [{"id": "c3", "name": "edit", "args": {"path": "x.txt", "edits": [
        {"old": "", "new": "one\\ntwo\\n"},
        {"old": "two", "new": "TWO"}]}}],
    [{"id": "c4", "name": "bash", "args": {"command": "git status --short"}}],
]

MODELS = json.dumps({"data": [{"id": "test/model", "context_length": 262144,
                               "supported_parameters": ["tools"]}]}).encode()


def sse(chunks):
    body = "".join(f"data: {json.dumps(chunk)}\n\n" for chunk in chunks)
    return (body + "data: [DONE]\n\n").encode()


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def __init__(self, *args, **kwargs):
        self.folder = kwargs.pop("folder")
        self.count = kwargs.pop("count")
        super().__init__(*args, **kwargs)

    def do_GET(self):
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(MODELS)))
        self.end_headers()
        self.wfile.write(MODELS)

    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get("content-length", 0)))
        self.count[0] += 1
        index = self.count[0]
        # The bytes as they arrived, before any parsing: that is the cost.
        with open(os.path.join(self.folder, f"req-{index}.json"), "wb") as handle:
            handle.write(raw)
        if index <= len(TURNS):
            calls = [{"index": k, "id": call["id"], "type": "function",
                      "function": {"name": call["name"],
                                   "arguments": json.dumps(call["args"])}}
                     for k, call in enumerate(TURNS[index - 1])]
            chunks = [{"choices": [{"delta": {"tool_calls": calls}}]},
                      {"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}]
        else:
            chunks = [{"choices": [{"delta": {"content": "done"}}]},
                      {"choices": [{"delta": {}, "finish_reason": "stop"}]}]
        chunks.append({"choices": [], "usage": {"prompt_tokens": 10,
                                                "completion_tokens": 5, "cost": 0}})
        body = sse(chunks)
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


def free_port():
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


def measure(binary, label, work):
    folder = os.path.join(work, label)
    requests = os.path.join(folder, "requests")
    for name in ("work", "state", "config", "requests"):
        os.makedirs(os.path.join(folder, name), exist_ok=True)
    config = os.path.join(folder, "config", "config.toml")
    with open(config, "w", encoding="utf-8") as handle:
        handle.write('provider = "openrouter"\nmodel = "test/model"\n')

    port = free_port()
    count = [0]
    server = http.server.ThreadingHTTPServer(
        ("127.0.0.1", port),
        lambda *args, **kwargs: Handler(*args, folder=requests, count=count, **kwargs),
    )
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    env = dict(os.environ)
    env.update({
        "KELIR_CONFIG": config,
        "KELIR_CONFIG_DIR": os.path.join(folder, "config", "none"),
        "KELIR_STATE_DIR": os.path.join(folder, "state"),
        "KELIR_HOME": os.path.join(folder, "home"),
        "KELIR_BASE_URL": f"http://127.0.0.1:{port}/v1",
        "OPENROUTER_API_KEY": "bench",
    })
    for name in ("KELIR_SESSION_DIR", "KELIR_SOCK", "KELIR_CALL_ID", "KELIR_BIN"):
        env.pop(name, None)
    try:
        subprocess.run([binary, "run", "--json", "the fixed benchmark task"],
                       cwd=os.path.join(folder, "work"), env=env, timeout=60,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)

    files = sorted(glob.glob(os.path.join(requests, "req-*.json")),
                   key=lambda path: int(path.rsplit("-", 1)[1].split(".")[0]))
    if not files:
        print(f"{label}: no requests recorded", file=sys.stderr)
        return None
    total = 0
    first_system = None
    first_tools = None
    for index, path in enumerate(files, start=1):
        raw = open(path, "rb").read()
        total += len(raw)
        body = json.loads(raw)
        messages = body.get("messages") or []
        tools = json.dumps(body.get("tools") or [], separators=(",", ":"))
        print(f"{label}: request {index}: body={len(raw)} "
              f"messages={len(json.dumps(messages, separators=(',', ':')))} "
              f"tools={len(tools)}")
        if index == 1:
            first_system = len(((messages[0].get("content") if messages else "") or "")
                               .encode())
            first_tools = len(tools)
    print(f"{label}: system={first_system} tools={first_tools} "
          f"total={total} requests={len(files)}")
    return {"system": first_system, "tools": first_tools, "total": total,
            "requests": len(files)}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--old", required=True)
    parser.add_argument("--new", required=True)
    args = parser.parse_args()
    work = tempfile.mkdtemp(prefix="kelir-steer6-bench-")
    try:
        print("== old binary")
        old = measure(args.old, "old", work)
        print("== new binary")
        new = measure(args.new, "new", work)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    if old and new:
        print()
        print(f"system prompt: {old['system']} -> {new['system']} bytes "
              f"({new['system'] - old['system']:+d})")
        print(f"tool schemas:  {old['tools']} -> {new['tools']} bytes "
              f"({new['tools'] - old['tools']:+d})")
        print(f"request bytes: {old['total']} -> {new['total']} "
              f"({new['total'] - old['total']:+d}) over {new['requests']} requests")
        print("system and tools are frozen per session (paid once); every step "
              "re-sends the messages array.")


if __name__ == "__main__":
    main()
