#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""Writes the tui2b round-2 scenario specs (for tui2_scn.py) into <outdir>.
usage: tui2b_specs.py <outdir> <work-dir-prefix-for-image-paths>
Run each with:  TUI2_OUT=<scratch> uv run --script scripts/live/tui2_scn.py <name> <outdir>/<name>.json [--light]
Names: ord100/ord60/ord40 (calls->answer, answer->calls, interleaved), exhaust (provider exhaustion -> paused ->
continue), rlstop (429 countdown, Esc during the wait), rlsay (429 countdown, plain 'stop' typed), mds7_<cols>
(loose lists, nested continuation, fence in an item, narrow tables)."""
import json, os, sys

out = sys.argv[1]
os.makedirs(out, exist_ok=True)
start = [{"wait_for": "kelir", "timeout": 20}, {"wait": 1.0}]
quit_ = [{"send": "/quit"}, {"wait": 0.3}, {"send": "\r"}]


def say(t):
    return [{"send": t}, {"wait": 0.3}, {"send": "\r"}]


def w(name, d):
    json.dump(d, open(os.path.join(out, name + ".json"), "w"))


def tool(i, c):
    return {"tool": {"id": i, "name": "bash", "args": {"command": c}}}


def text(t):
    return {"text": t, "chunk": 8, "delay": 0.03}


turns = [[tool("a1", "echo alpha"), tool("a2", "echo beta")], [text("Both commands ran: alpha and beta.\n")],
         [text("Now I will run two more.\n"), tool("b1", "echo gamma"), tool("b2", "echo delta")], [text("Gamma and delta done.\n")],
         [text("First I check one."), tool("c1", "echo epsilon")],
         [text("Then the second part of the thought.\n\nSecond paragraph."), tool("c2", "echo zeta")], [text("Finished the interleaved turn.\n")]]
steps = (start + say("one") + [{"wait_for": "alpha and beta", "timeout": 30}, {"wait": 1.5}, {"snap": "t1"}] + say("two")
         + [{"wait_for": "Gamma and delta done", "timeout": 30}, {"wait": 1.5}, {"snap": "t2"}] + say("three")
         + [{"wait_for": "interleaved turn", "timeout": 30}, {"wait": 1.5}, {"snap": "t3"}] + quit_)
for c in (100, 60, 40):
    w("ord%d" % c, dict(turns=turns, steps=steps, rows=60, cols=c))
bad = {"status": 503, "body": "{\"error\":{\"message\":\"overloaded\"}}"}
w("exhaust", dict(turns=[[bad], [bad], [bad], [{"text": "back at last\n"}]], cfg="retry_base_ms = 700\nretry_cap_ms = 700\nretry_max_attempts = 2\n",
                  steps=start + say("hi") + [{"wait": 6}, {"snap": "paused"}] + say("continue") + [{"wait": 4}, {"snap": "after"}] + quit_, rows=30, cols=100))
rl = [{"status": 429, "body": "{\"error\":{\"message\":\"slow\"}}"}]
w("rlstop", dict(turns=[rl, rl, [{"text": "never shown\n"}]], cfg="retry_base_ms = 8000\nretry_cap_ms = 8000\n",
                 steps=start + say("hi") + [{"wait": 2.5}, {"snap": "w1"}, {"wait": 2}, {"snap": "w2"}, {"send": "\u001b"}, {"wait": 1.5}, {"snap": "stopped"}] + quit_,
                 rows=30, cols=100, timeout=120))
w("rlsay", dict(turns=[rl, [{"text": "answer after the retry\n"}]], cfg="retry_base_ms = 5000\nretry_cap_ms = 5000\n",
                steps=start + say("hi") + [{"wait": 1.5}, {"snap": "w1"}] + say("stop") + [{"wait": 2}, {"snap": "w2"}] + quit_, rows=30, cols=100, timeout=120))
md = ("# Loose lists\n\n1. First item of the list\n\n   Continuation paragraph of the first item that is long enough to wrap around the pane.\n\n"
      "   - nested a\n\n     nested a continuation paragraph\n\n   - nested b\n\n2. Second item\n\n   ```rust\n   let x = 1;\n   ```\n\n   tail paragraph of second\n\n"
      "After the list.\n\n| Name | Description | Status |\n|---|---|---|\n| alpha | a rather long description that must wrap around | ok |\n| beta | wide | fail |\n\nFinal paragraph.\n")
for c in (100, 60, 40, 30):
    w("mds7_%d" % c, dict(turns=[[{"text": md, "chunk": 9, "delay": 0.02}]], steps=start + say("md") + [{"wait_for": "Final paragraph", "timeout": 40}, {"wait": 1.5}, {"snap": "done"}] + quit_, rows=60, cols=c))
print("wrote specs to", out)
