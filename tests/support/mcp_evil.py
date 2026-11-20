#!/usr/bin/env python3
"""A misbehaving MCP server for the M6 review. Standard library only.

Behaviour is chosen by MCP_EVIL (comma separated):
  banner      print non-JSON junk lines on stdout before the first message
  badutf8     before answering tools/call, print a line that is not valid UTF-8
  hang_init   never answer initialize
  init_error  answer initialize with a JSON-RPC error
  exit_init   exit right after the initialize answer
  strid       answer tools/call with the id as a string ("N"), so it never matches
  noresult    answer tools/call with neither result nor error
  stderr      flood stderr (8 MB) at start
  crlf        end every line with \r\n
  unicode     the tool list holds "héllo wörld 日本" (a name with spaces)
  hugeN       tools/call `huge` returns N megabytes of text (default 12)
  listchanged after the first tools/call send notifications/tools/list_changed and add a tool
  progress    send notifications/progress before the tools/call answer
  request     send a server->client request (roots/list) before the answer
  splitwrite  write each answer in 3 separate flushed chunks with pauses
  oldproto    answer initialize with protocolVersion 2024-11-05
"""
import json
import os
import sys
import time

MODES = set(filter(None, os.environ.get("MCP_EVIL", "").split(",")))
EOL = "\r\n" if "crlf" in MODES else "\n"

OUT = sys.stdout.buffer


def raw(data: bytes):
    OUT.write(data)
    OUT.flush()


def send(message):
    data = (json.dumps(message, separators=(",", ":"), ensure_ascii=False) + EOL).encode("utf-8")
    if "splitwrite" in MODES:
        third = max(1, len(data) // 3)
        for i in range(0, len(data), third):
            raw(data[i : i + third])
            time.sleep(0.05)
    else:
        raw(data)


def tools():
    name = "héllo wörld 日本" if "unicode" in MODES else "echo"
    out = [
        {
            "name": name,
            "description": "echo args",
            "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
        },
        {
            "name": "nullable",
            "description": "ids that are strings or null, written three ways",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "a": {"anyOf": [{"type": "string"}, {"type": "null"}]},
                    "b": {"type": ["string", "null"]},
                    "c": {"type": "string"},
                    "d": {},
                },
            },
        },
        {"name": "huge", "description": "big", "inputSchema": {"type": "object"}},
        {"name": "ls", "description": "a tool that is literally called ls", "inputSchema": {"type": "object"}},
    ]
    return out


calls = 0


def main():
    global calls
    if "stderr" in MODES:
        sys.stderr.write("E" * (8 * 1024 * 1024))
        sys.stderr.flush()
    if "banner" in MODES:
        raw(b"Welcome to the evil server v1\n")
        raw(b"{not json at all\n")
        raw(b"\n")
    stdin = sys.stdin.buffer
    for line in stdin:
        line = line.strip()
        if not line:
            continue
        message = json.loads(line)
        method = message.get("method")
        mid = message.get("id")
        if method == "initialize":
            if "hang_init" in MODES:
                time.sleep(10000)
            if "init_error" in MODES:
                send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32000, "message": "go away"}})
                continue
            version = "2024-11-05" if "oldproto" in MODES else message["params"]["protocolVersion"]
            send(
                {
                    "jsonrpc": "2.0",
                    "id": mid,
                    "result": {
                        "protocolVersion": version,
                        "capabilities": {"tools": {"listChanged": True}},
                        "serverInfo": {"name": "evil", "version": "0"},
                    },
                }
            )
            if "exit_init" in MODES:
                os._exit(0)
        elif method == "tools/list":
            send({"jsonrpc": "2.0", "id": mid, "result": {"tools": tools()}})
        elif method == "tools/call":
            calls += 1
            params = message.get("params", {})
            name = params.get("name")
            if "badutf8" in MODES:
                raw(b"\xff\xfe not utf8 \x80\n")
            if "progress" in MODES:
                send({"jsonrpc": "2.0", "method": "notifications/progress", "params": {"progressToken": 1, "progress": 1}})
            if "request" in MODES:
                send({"jsonrpc": "2.0", "id": 900, "method": "roots/list"})
            if "noresult" in MODES:
                send({"jsonrpc": "2.0", "id": mid})
                continue
            if "strid" in MODES:
                send({"jsonrpc": "2.0", "id": str(mid), "result": {"content": [{"type": "text", "text": "x"}]}})
                continue
            if name == "huge":
                mb = 12
                for mode in MODES:
                    if mode.startswith("huge") and mode[4:].isdigit():
                        mb = int(mode[4:])
                text = ("0123456789abcdef" * 63 + "012345678901234\n") * (mb * 1024)
                send({"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text", "text": text}]}})
                continue
            args = params.get("arguments")
            send(
                {
                    "jsonrpc": "2.0",
                    "id": mid,
                    "result": {"content": [{"type": "text", "text": json.dumps(args, sort_keys=True, ensure_ascii=False)}]},
                }
            )
            if "listchanged" in MODES and calls == 1:
                send({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
        elif method == "ping":
            send({"jsonrpc": "2.0", "id": mid, "result": {}})
        elif mid is not None:
            send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "method not found"}})


if __name__ == "__main__":
    main()
