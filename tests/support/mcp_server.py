#!/usr/bin/env python3
"""A tiny MCP server over stdio for kelir's M6 tests. Standard library only.

Tools: echo, fail (isError), slow (sleeps), big (20 KB), image (1x1 PNG),
structured (structuredContent), pid (this process), crash (dies mid-call).

Environment (all optional):
  MCP_TEST_LOG       append one line per tools/call: "<tool> <pid>"
  MCP_TEST_PIDFILE   write this process's pid there at start
  MCP_TEST_CHANNEL   send one notifications/claude/channel with this content
                     right after the client says "initialized"
  MCP_TEST_NO_TOOLS  answer tools/list with a -32601 error
  MCP_TEST_DIE       write this text to stderr at start and exit 1
                     (the value `key` writes "boom: missing API key")
  MCP_TEST_LISTCHANGED  after the first tools/call add a tool `late` and
                     send notifications/tools/list_changed
"""
import json
import os
import sys
import time

PNG = (
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmM"
    "IQAAAABJRU5ErkJggg=="
)

TOOLS = [
    {
        "name": "echo",
        "description": "Echo the text back.\nSecond line of the description.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "text": {"type": "string"},
                "n": {"type": "integer", "minimum": 0},
                "mode": {"enum": ["plain", "loud"]},
            },
            "required": ["text"],
            "additionalProperties": False,
        },
    },
    {"name": "fail", "description": "Always reports isError.", "inputSchema": {"type": "object"}},
    {
        "name": "slow",
        "description": "Sleep for `seconds` (default 30).",
        "inputSchema": {"type": "object", "properties": {"seconds": {"type": "number"}}},
    },
    {"name": "big", "description": "A 20 KB result.", "inputSchema": {"type": "object"}},
    {"name": "image", "description": "A 1x1 PNG.", "inputSchema": {"type": "object"}},
    {"name": "structured", "description": "structuredContent.", "inputSchema": {"type": "object"}},
    {"name": "pid", "description": "This server's pid.", "inputSchema": {"type": "object"}},
    {"name": "crash", "description": "Exit mid-call.", "inputSchema": {"type": "object"}},
    {"name": "envnames", "description": "Names of the environment variables.", "inputSchema": {"type": "object"}},
]
LATE = {"name": "late", "description": "Appears after list_changed.", "inputSchema": {"type": "object"}}
calls = 0


def send(message):
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def text(value, error=False):
    result = {"content": [{"type": "text", "text": value}]}
    if error:
        result["isError"] = True
    return result


def call(name, arguments):
    log = os.environ.get("MCP_TEST_LOG")
    if log:
        with open(log, "a", encoding="utf-8") as handle:
            handle.write("%s %d\n" % (name, os.getpid()))
    if name == "echo":
        out = arguments.get("text", "")
        if arguments.get("mode") == "loud":
            out = out.upper()
        return text(out * int(arguments.get("n", 1) or 1) if "n" in arguments else out)
    if name == "fail":
        return text("the tool says no", error=True)
    if name == "slow":
        time.sleep(float(arguments.get("seconds", 30)))
        return text("woke")
    if name == "big":
        line = "0123456789abcdef" * 4 + "\n"
        return text(line * (20000 // len(line) + 1))
    if name == "image":
        return {
            "content": [
                {"type": "text", "text": "an image"},
                {"type": "image", "data": PNG, "mimeType": "image/png"},
            ]
        }
    if name == "structured":
        return {
            "content": [{"type": "text", "text": "{\"answer\": 42}"}],
            "structuredContent": {"answer": 42, "list": [1, 2]},
        }
    if name == "pid":
        return text(str(os.getpid()))
    if name == "crash":
        os._exit(7)
    if name == "envnames":
        return text(chr(10).join(sorted(os.environ)))
    if name == "late":
        return text("late ok")
    return None


def main():
    sys.stdout.reconfigure(newline="\n")
    die = os.environ.get("MCP_TEST_DIE")
    if die:
        sys.stderr.write(die + "\n")
        sys.stderr.flush()
        sys.exit(1)
    pidfile = os.environ.get("MCP_TEST_PIDFILE")
    if pidfile:
        with open(pidfile, "w", encoding="utf-8") as handle:
            handle.write(str(os.getpid()))
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        message = json.loads(line)
        method = message.get("method")
        mid = message.get("id")
        if method == "initialize":
            send(
                {
                    "jsonrpc": "2.0",
                    "id": mid,
                    "result": {
                        "protocolVersion": message["params"]["protocolVersion"],
                        "capabilities": {
                            "tools": {},
                            "experimental": {"claude/channel": {}},
                        },
                        "serverInfo": {"name": "kelir-test-server", "version": "0"},
                    },
                }
            )
        elif method == "notifications/initialized":
            channel = os.environ.get("MCP_TEST_CHANNEL")
            if channel:
                send(
                    {
                        "jsonrpc": "2.0",
                        "method": "notifications/claude/channel",
                        "params": {"content": channel, "meta": {"chat": "t1"}},
                    }
                )
        elif method == "tools/list":
            if os.environ.get("MCP_TEST_NO_TOOLS"):
                send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "no tools"}})
            else:
                send({"jsonrpc": "2.0", "id": mid, "result": {"tools": TOOLS}})
        elif method == "tools/call":
            params = message.get("params", {})
            result = call(params.get("name"), params.get("arguments") or {})
            global calls
            calls += 1
            if result is None:
                send(
                    {
                        "jsonrpc": "2.0",
                        "id": mid,
                        "error": {"code": -32602, "message": "unknown tool " + str(params.get("name"))},
                    }
                )
            else:
                send({"jsonrpc": "2.0", "id": mid, "result": result})
                if os.environ.get("MCP_TEST_LISTCHANGED") and calls == 1:
                    TOOLS.append(LATE)
                    send({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
        elif method == "ping":
            send({"jsonrpc": "2.0", "id": mid, "result": {}})
        elif mid is not None:
            send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "method not found"}})


if __name__ == "__main__":
    main()
