#!/usr/bin/env python3
"""A fake chat bridge: an MCP server that sends channel notifications.

Standard library only. Driven by files so a test can inject messages while a
`kelir channel` process is running:

  MCP_CHAN_INBOX   file of JSON messages (one per line); the tail is polled
  MCP_CHAN_REPLIES file to append one JSON line per reply tool call
  MCP_CHAN_EVENTS  file to append one JSON line per notification/initialize
  MCP_CHAN_SECRETS secret canary that must never appear in a kelir log

The `reply` tool is idempotent by message_id: a repeated id is logged with
`duplicate: true`. `turn_done` notifications are appended to the events file.
"""
import json
import os
import sys
import time

sent = set()
inbox = os.environ.get("MCP_CHAN_INBOX", "")
replies = os.environ.get("MCP_CHAN_REPLIES", "")
events = os.environ.get("MCP_CHAN_EVENTS", "")
offset = 0
last_check = 0.0


def send(message):
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def note(kind, **fields):
    if events:
        record = {"kind": kind}
        record.update(fields)
        with open(events, "a", encoding="utf-8") as handle:
            handle.write(json.dumps(record, separators=(",", ":")) + "\n")


def tools():
    return [{
        "name": "reply",
        "description": "Send one message back to the conversation.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "conversation_id": {"type": "string"},
                "text": {"type": "string"},
                "message_id": {"type": "string"},
                "reply_to": {"type": "string"},
            },
            "required": ["conversation_id", "text"],
        },
    }]


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
        send({
            "jsonrpc": "2.0",
            "method": "notifications/claude/channel",
            "params": message,
        })
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
            send({
                "jsonrpc": "2.0", "id": mid,
                "result": {
                    "protocolVersion": (message.get("params") or {}).get("protocolVersion", "2024-11-05"),
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fake-channel", "version": "0"},
                },
            })
        elif method == "notifications/initialized":
            poll()
        elif method == "tools/list":
            send({"jsonrpc": "2.0", "id": mid, "result": {"tools": tools()}})
        elif method == "tools/call":
            params = message.get("params") or {}
            name = params.get("name")
            args = params.get("arguments") or {}
            if name == "reply":
                ident = str(args.get("message_id", ""))
                duplicate = ident in sent
                sent.add(ident)
                if replies:
                    with open(replies, "a", encoding="utf-8") as handle:
                        handle.write(json.dumps({"id": ident, "text": args.get("text", ""),
                                                  "conversation": args.get("conversation_id", ""),
                                                  "duplicate": duplicate}, separators=(",", ":")) + "\n")
                send({"jsonrpc": "2.0", "id": mid, "result": {"content": [
                    {"type": "text", "text": "sent"}]}})
            else:
                send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32602,
                      "message": "unknown tool"}})
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
