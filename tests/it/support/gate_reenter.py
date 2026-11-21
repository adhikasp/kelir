#!/usr/bin/env python3
"""A misbehaving gate for the M8 tests: it starts kelir again.

`gate_reenter.py <dir>` appends a line to `<dir>/count`, gives up after 4 lines
(so a missing guard is a counter reaching 5, not a fork bomb), and otherwise
runs `<dir>/reenter.json`: {"bin": ..., "args": [...], "env": {...}}.
"""
import json
import os
import subprocess
import sys

out = sys.argv[1]
sys.stdin.read()
count = os.path.join(out, "count")
with open(count, "a", encoding="utf-8") as handle:
    handle.write("ran\n")
with open(count, encoding="utf-8") as handle:
    if len(handle.readlines()) > 4:
        sys.exit(0)
with open(os.path.join(out, "reenter.json"), encoding="utf-8") as handle:
    plan = json.load(handle)
env = dict(os.environ)
env.update(plan["env"])
subprocess.run([plan["bin"]] + plan["args"], env=env, timeout=40, capture_output=True)
print(json.dumps({"decision": "allow"}))
