#!/usr/bin/env python3
"""A scriptable gate for the M8 tests: `gate_probe.py <dir>`.

Records what it was given (`<dir>/<event>.jsonl`: the stdin document, one per
line; `<dir>/env-<event>.json`: its environment) and then does what
`<dir>/plan.json` says for the event: {"<event>": {"mode": ..., ...}}.
Modes: allow (default), deny{reason}, rewrite{input}, crash, garbage, empty,
hang{flag}: sleep for a minute after starting a child that would write `flag`
after 3 s (so a surviving child is visible).
"""
import json
import os
import subprocess
import sys
import time

out = sys.argv[1]
raw = sys.stdin.read()
event = json.loads(raw)
name = event["event"]
with open(os.path.join(out, name + ".jsonl"), "a", encoding="utf-8") as handle:
    handle.write(raw.replace("\n", " ") + "\n")
with open(os.path.join(out, "env-" + name + ".json"), "w", encoding="utf-8") as handle:
    json.dump(dict(os.environ), handle)
plan = {}
try:
    with open(os.path.join(out, "plan.json"), encoding="utf-8") as handle:
        plan = json.load(handle)
except OSError:
    pass
step = plan.get(name) or {}
mode = step.get("mode", "allow")
if mode == "deny":
    print(json.dumps({"decision": "deny", "reason": step.get("reason", "")}))
elif mode == "rewrite":
    print(json.dumps({"decision": "rewrite", "reason": step.get("reason", ""), "input": step["input"]}))
elif mode == "crash":
    sys.stderr.write("probe crashed on purpose\n")
    sys.exit(3)
elif mode == "garbage":
    print("this is not json")
elif mode == "empty":
    pass
elif mode == "hang":
    code = "import time;time.sleep(3);open(%r,'w').write('alive')" % step["flag"]
    subprocess.Popen([sys.executable, "-c", code])
    time.sleep(60)
else:
    print(json.dumps({"decision": "allow"}))
