#!/usr/bin/env python3
"""Reviewer's fake channel bridge: an MCP server that records reply tool args.

Like tests/support/channel_server.py but also writes every `reply` argument
object verbatim to MCP_CHAN_ARGS, so a reviewer test can assert threading,
fallback counts and idempotency keys. Standard library only.
"""
import json
import os
import sys
import time

inbox = os.environ.get("MCP_CHAN_INBOX", "")
replies = os.environ.get("MCP_CHAN_REPLIES", "")
args_path = os.environ.get("MCP_CHAN_ARGS", "")
events = os.environ.get("MCP_CHAN_EVENTS", "")
offset = 0
last_check = 0.0
sent = set()


def send(message):
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def note(kind, **fields):
    if events:
        record = {"kind": kind}
        record.update(fields)
        with open(events, "a", encoding="utf-8") as handle:
            handle.write(json.dumps(record, separators=(",", ":")) + "\n")


def poll():
    global offset, last_check
    now = time.monotonic()
    if now - last_check < 0.05:
        return
    last_check = now
    if not inbox:
        return
    try:
        with open(inbox, "r", encoding="utf-8") as handle:
            handle.seek(offset)
            chunk = handle.read()
            offset = handle.tell()
    except OSError:
        return
    for line in chunk.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except ValueError:
            continue
        send({"jsonrpc": "2.0", "method": "notifications/claude/channel",
              "params": message})
        note("inbound", message_id=message.get("message_id", ""))


def main():
    sys.stdout.reconfigure(newline="\n")
    for line in sys.stdin:
        poll()
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except ValueError:
            continue
        method = message.get("method")
        mid = message.get("id")
        if method == "initialize":
            caps = (message.get("params") or {}).get("capabilities") or {}
            note("initialize", capabilities=caps)
            send({"jsonrpc": "2.0", "id": mid, "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "review-channel", "version": "0"}}})
        elif method == "notifications/initialized":
            poll()
        elif method == "tools/list":
            schema = {
                "type": "object",
                "properties": {
                    "conversation_id": {"type": "string"},
                    "text": {"type": "string"},
                    "message_id": {"type": "string"},
                    "reply_to": {"type": "string"},
                },
            }
            tool = {"name": "reply", "description": "reply", "inputSchema": schema}
            send({"jsonrpc": "2.0", "id": mid, "result": {"tools": [tool]}})
        elif method == "tools/call":
            params = message.get("params") or {}
            name = params.get("name")
            arguments = params.get("arguments") or {}
            if name == "reply":
                ident = str(arguments.get("message_id", ""))
                duplicate = ident in sent
                sent.add(ident)
                if args_path:
                    with open(args_path, "a", encoding="utf-8") as handle:
                        handle.write(json.dumps(arguments, separators=(",", ":")) + "\n")
                if replies:
                    with open(replies, "a", encoding="utf-8") as handle:
                        handle.write(json.dumps({"id": ident,
                                                 "text": arguments.get("text", ""),
                                                 "duplicate": duplicate},
                                                separators=(",", ":")) + "\n")
                send({"jsonrpc": "2.0", "id": mid,
                      "result": {"content": [{"type": "text", "text": "sent"}]}})
            else:
                send({"jsonrpc": "2.0", "id": mid,
                      "error": {"code": -32602, "message": "unknown tool"}})
        elif method and method.startswith("notifications/claude/channel/turn_done"):
            note("turn_done", params=message.get("params") or {})
        elif method == "ping":
            send({"jsonrpc": "2.0", "id": mid, "result": {}})
        poll()


if __name__ == "__main__":
    try:
        main()
    except BrokenPipeError:
        pass
