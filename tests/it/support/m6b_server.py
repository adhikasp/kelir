#!/usr/bin/env python3
"""MCP test server for the m6b review (reviewer-owned). Standard library only.

Tools:
  argv      the server's own sys.argv and pid, as text
  leak      the value of $M6B_SECRET, as a tool RESULT (the secret in a result)
  spawnkid  start a grandchild (sleeps 300 s), write its pid to $M6B_KIDFILE
  hold      sleep `seconds` (default 120) without reading stdin (busy mid-call)
  cmdline   the full command line of the pid in `pid` (Windows: CIM, Linux: /proc)
Environment:
  M6B_PIDFILE      write own pid there at start
  M6B_IGNORE_EOF   when stdin closes, keep running (a server that does not exit on EOF)
  M6B_DIE_ECHO     at start print $M6B_SECRET to stderr and exit 1 (a server that
                   echoes its configuration when it fails)
  M6B_CHANNEL      send a notifications/claude/channel with this content after initialized
"""
import json
import os
import subprocess
import sys
import time

TOOLS = [
    {"name": "argv", "description": "argv", "inputSchema": {"type": "object"}},
    {"name": "leak", "description": "return the secret", "inputSchema": {"type": "object"}},
    {"name": "spawnkid", "description": "grandchild", "inputSchema": {"type": "object"}},
    {"name": "hold", "description": "busy", "inputSchema": {"type": "object"}},
    {"name": "cmdline", "description": "cmdline of a pid", "inputSchema": {"type": "object"}},
    {
        "name": "echo",
        "description": "echo",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}, "token": {"type": "string"}},
        },
    },
]


def send(message):
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def text(value, error=False):
    result = {"content": [{"type": "text", "text": value}]}
    if error:
        result["isError"] = True
    return result


def call(name, arguments):
    if name == "argv":
        return text(json.dumps({"argv": sys.argv, "pid": os.getpid()}))
    if name == "leak":
        return text("the secret is " + os.environ.get("M6B_SECRET", "<unset>"))
    if name == "echo":
        return text("echo: " + json.dumps(arguments))
    if name == "spawnkid":
        kid = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(300)"])
        path = os.environ.get("M6B_KIDFILE")
        if path:
            with open(path, "w", encoding="utf-8") as handle:
                handle.write(str(kid.pid))
        return text(str(kid.pid))
    if name == "hold":
        time.sleep(float(arguments.get("seconds", 120)))
        return text("released")
    if name == "cmdline":
        pid = int(arguments["pid"])
        if os.name == "nt":
            out = subprocess.run(
                [
                    "powershell",
                    "-NoProfile",
                    "-Command",
                    "(Get-CimInstance Win32_Process -Filter 'ProcessId=%d').CommandLine" % pid,
                ],
                capture_output=True,
                text=True,
            ).stdout
        else:
            with open("/proc/%d/cmdline" % pid, "rb") as handle:
                out = handle.read().replace(b"\0", b" ").decode("utf-8", "replace")
        return text(out.strip())
    return None


def main():
    sys.stdout.reconfigure(newline="\n")
    if os.environ.get("M6B_DIE_ECHO"):
        sys.stderr.write("fatal: bad credentials " + os.environ.get("M6B_SECRET", "") + "\n")
        sys.stderr.flush()
        sys.exit(1)
    pidfile = os.environ.get("M6B_PIDFILE")
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
                        "capabilities": {"tools": {}, "experimental": {"claude/channel": {}}},
                        "serverInfo": {"name": "m6b", "version": "0"},
                    },
                }
            )
        elif method == "notifications/initialized":
            channel = os.environ.get("M6B_CHANNEL")
            if channel:
                send(
                    {
                        "jsonrpc": "2.0",
                        "method": "notifications/claude/channel",
                        "params": {"content": channel, "meta": {"chat": "t1"}},
                    }
                )
        elif method == "tools/list":
            send({"jsonrpc": "2.0", "id": mid, "result": {"tools": TOOLS}})
        elif method == "tools/call":
            params = message.get("params", {})
            result = call(params.get("name"), params.get("arguments") or {})
            if result is None:
                send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32602, "message": "unknown tool"}})
            else:
                send({"jsonrpc": "2.0", "id": mid, "result": result})
        elif method == "ping":
            send({"jsonrpc": "2.0", "id": mid, "result": {}})
        elif mid is not None:
            send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "method not found"}})
    if os.environ.get("M6B_IGNORE_EOF"):
        # The TypeScript SDK's stdio transport does not exit on stdin EOF.
        while True:
            time.sleep(1)


if __name__ == "__main__":
    main()
