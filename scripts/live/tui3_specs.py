#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""tui3 review scenario specs for tui2_scn.py. usage: tui3_specs.py <outdir>
Run each:  TUI2_OUT=<scratch> uv run --script scripts/live/tui2_scn.py <name> <outdir>/<name>.json [--light]
Names: fold3_<cols> (3 calls then answer), fold5f (5 calls, one failed, one edit), two (2 calls), ans_calls (answer then
3 calls then answer), inter (call/prose/call/prose), running (3 calls then a slow 4th across steps), paused (step cap),
err401, stopped (Esc in a slow call), rl (429 wait), resize (fold then 100->40->100), nocolor, ascii."""
import json, os, sys

out = sys.argv[1]
os.makedirs(out, exist_ok=True)
start = [{"wait_for": "kelir", "timeout": 20}, {"wait": 1.0}]
quit_ = [{"send": "/quit"}, {"wait": 0.3}, {"send": "\r"}]
say = lambda t: [{"send": t}, {"wait": 0.3}, {"send": "\r"}]
w = lambda n, d: json.dump(d, open(os.path.join(out, n + ".json"), "w"))
tool = lambda i, c: {"tool": {"id": i, "name": "bash", "args": {"command": c}}}
text = lambda t: {"text": t, "chunk": 8, "delay": 0.03}
three = [[tool("a1", "echo alpha"), tool("a2", "echo beta"), tool("a3", "echo gamma")], [text("All three ran: alpha, beta, gamma.\n")]]
done3 = start + say("run three") + [{"wait_for": "All three ran", "timeout": 30}, {"wait": 1.5}, {"snap": "done"}] + quit_
for c in (100, 80, 60, 40):
    w("fold3_%d" % c, dict(turns=three, steps=done3, rows=40, cols=c))
w("fold5f", dict(turns=[[tool("b1", "echo one"), tool("b2", "echo two && exit 3"), tool("b3", "echo three"),
                         {"tool": {"id": "b4", "name": "edit", "args": {"path": "notes.md", "old": "hello", "new": "world"}}},
                         tool("b5", "false")], [text("Mixed results above.\n")]],
                 files={"notes.md": "hello\n"}, rows=40, cols=80,
                 steps=start + say("mixed") + [{"wait_for": "Mixed results", "timeout": 30}, {"wait": 1.5}, {"snap": "done"}, {"send": "/expand"}, {"wait": 0.3}, {"send": "\r"}, {"wait": 1.0}, {"snap": "expand"}, {"send": "/expand 2"}, {"wait": 0.3}, {"send": "\r"}, {"wait": 1.0}, {"snap": "expand2"}] + quit_))
w("two", dict(turns=[[tool("c1", "echo one"), tool("c2", "echo two")], [text("Two calls only.\n")]], rows=40, cols=80,
              steps=start + say("two") + [{"wait_for": "Two calls only", "timeout": 30}, {"wait": 1.5}, {"snap": "done"}] + quit_))
w("ans_calls", dict(turns=[[text("I will check three things.\n"), tool("d1", "echo x1"), tool("d2", "echo x2"), tool("d3", "echo x3")], [text("Checked, all fine.\n")]],
                    rows=40, cols=80, steps=start + say("go") + [{"wait_for": "Checked, all fine", "timeout": 30}, {"wait": 1.5}, {"snap": "done"}] + quit_))
w("inter", dict(turns=[[text("First I look."), tool("e1", "echo e1")], [text("Then more thought.\n\nSecond para."), tool("e2", "echo e2")], [text("Last call next."), tool("e3", "echo e3"), tool("e4", "echo e4"), tool("e5", "echo e5")], [text("Finished the interleaved turn.\n")]],
                rows=50, cols=80, steps=start + say("go") + [{"wait_for": "interleaved turn", "timeout": 40}, {"wait": 1.5}, {"snap": "done"}] + quit_))
w("running", dict(turns=[[tool("f1", "echo r1"), tool("f2", "echo r2"), tool("f3", "echo r3")], [tool("f4", "sleep 6")], [text("Slow one finished.\n")]],
                  rows=30, cols=80, timeout=90, steps=start + say("go") + [{"wait": 3.0}, {"snap": "mid1"}, {"wait": 1.5}, {"snap": "mid2"}, {"wait_for": "Slow one finished", "timeout": 40}, {"wait": 1.5}, {"snap": "done"}] + quit_))
w("running_same", dict(turns=[[tool("g1", "echo r1"), tool("g2", "echo r2"), tool("g3", "echo r3"), tool("g4", "sleep 6")], [text("Slow one finished.\n")]],
                  rows=30, cols=80, timeout=90, steps=start + say("go") + [{"wait": 3.0}, {"snap": "mid1"}, {"wait_for": "Slow one finished", "timeout": 40}, {"wait": 1.5}, {"snap": "done"}] + quit_))
w("paused", dict(turns=[[tool("p1", "echo p1")], [tool("p2", "echo p2")], [tool("p3", "echo p3")], [text("never\n")]], cfg="max_steps = 2\n", rows=30, cols=80, timeout=60,
                 steps=start + say("go") + [{"wait": 6}, {"snap": "paused"}] + quit_))
w("err401", dict(turns=[[{"status": 401, "body": "{\"error\":{\"message\":\"bad key\"}}"}]], rows=30, cols=80, timeout=60,
                 steps=start + say("hi") + [{"wait": 3}, {"snap": "err"}] + quit_))
w("stopped", dict(turns=[[tool("s1", "echo s1"), tool("s2", "sleep 20")], [text("never\n")]], rows=30, cols=80, timeout=60,
                  steps=start + say("go") + [{"wait": 3}, {"snap": "run"}, {"send": "\u001b"}, {"wait": 2}, {"snap": "stopped"}] + quit_))
rl = [{"status": 429, "body": "{\"error\":{\"message\":\"slow\"}}"}]
w("rl", dict(turns=[rl, rl, [{"text": "never shown\n"}]], cfg="[retry]\nbase_ms = 8000\ncap_ms = 8000\n", rows=30, cols=80, timeout=60,
             steps=start + say("hi") + [{"wait": 2.5}, {"snap": "w1"}, {"send": "\u001b"}, {"wait": 1.5}, {"snap": "stopped"}] + quit_))
w("resize", dict(turns=three, rows=40, cols=100, steps=start + say("run three") + [{"wait_for": "All three ran", "timeout": 30}, {"wait": 1.0}, {"snap": "a100"}, {"resize": [40, 40]}, {"wait": 1.0}, {"snap": "b40"}, {"resize": [40, 100]}, {"wait": 1.0}, {"snap": "c100"}] + quit_))
w("nocolor", dict(turns=three, rows=40, cols=80, env={"NO_COLOR": "1"}, steps=done3))
w("ascii", dict(turns=three, rows=40, cols=80, env={"KELIR_ASCII": "1"}, steps=done3))
w("idle", dict(turns=three, rows=20, cols=80, steps=start + [{"snap": "idle"}] + quit_))
print("wrote", out)
