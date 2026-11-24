#!/usr/bin/env python3
"""Adversarial gate for the M8 review: `rev_gate.py <mode> <dir> [arg]`.

Always drains stdin first (except `noread`). Modes: flood (100 MB on stdout),
binary (invalid UTF-8 and NULs), deep (JSON nested far past any recursion limit),
bigreason (valid deny with a 900 KB reason), ignoreterm (a grandchild that
ignores SIGTERM and writes <dir>/alive after 3 s; this process hangs),
daemon (Unix: a setsid'd grandchild writing <dir>/daemon after 3 s; this exits
at once), hang (sleep 60 after writing <dir>/started), noread (never reads
stdin, sleeps 2 s), selfspawn (starts itself twice, depth-capped at 3 via
<dir>/depth-N markers, then sleeps 1 s), envdump (<dir>/env.json), allow.
"""
import json
import os
import signal
import subprocess
import sys
import time

mode, out = sys.argv[1], sys.argv[2]
if mode != "noread":
    sys.stdin.read()
GRAND = (
    "import signal,time,sys;"
    "signal.signal(signal.SIGTERM, signal.SIG_IGN) if hasattr(signal,'SIGTERM') else None;"
    "time.sleep(3);open(sys.argv[1],'w').write('alive')"
)
if mode == "flood":
    chunk = b"x" * (1 << 20)
    for _ in range(100):
        try:
            sys.stdout.buffer.write(chunk)
        except (BrokenPipeError, OSError):
            break
    sys.stdout.flush()
elif mode == "binary":
    sys.stdout.buffer.write(b"\x00\xff\xfe\x80{\"decision\":\"deny\"}\x00" + bytes(range(256)) * 64)
elif mode == "deep":
    sys.stdout.write("[" * 200000 + "]" * 200000)
elif mode == "bigreason":
    print(json.dumps({"decision": "deny", "reason": "R" * 900000}))
elif mode == "ignoreterm":
    subprocess.Popen([sys.executable, "-c", GRAND, os.path.join(out, "alive")])
    time.sleep(60)
elif mode == "daemon":
    kwargs = {"start_new_session": True} if os.name != "nt" else {}
    subprocess.Popen([sys.executable, "-c", GRAND, os.path.join(out, "daemon")],
                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                     stderr=subprocess.DEVNULL, **kwargs)
elif mode == "hang":
    open(os.path.join(out, "started"), "w").write(str(os.getpid()))
    time.sleep(60)
elif mode == "noread":
    time.sleep(2)
elif mode == "selfspawn":
    depth = int(sys.argv[3]) if len(sys.argv) > 3 else 0
    open(os.path.join(out, f"depth-{depth}-{os.getpid()}"), "w").write("x")
    if depth < 3:
        kids = [subprocess.Popen([sys.executable, __file__, "selfspawn", out, str(depth + 1)],
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL)
                for _ in range(2)]
        time.sleep(1)
        for kid in kids:
            kid.wait()
    else:
        time.sleep(1)
elif mode == "envdump":
    with open(os.path.join(out, "env.json"), "w", encoding="utf-8") as handle:
        json.dump(dict(os.environ), handle)
    print(json.dumps({"decision": "allow"}))
else:
    print(json.dumps({"decision": "allow"}))
