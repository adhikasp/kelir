import sys, os
sys.argv = ["x", sys.argv[1], sys.argv[2]]
import m7_drive as d
mine = [p for p in d.procs() if d.is_mine(p, sys.argv[2])]
for p in mine:
    print("kill", p["pid"], p["name"], p["cmd"][:90]); d.kill_pid(p["pid"])
print("killed", len(mine))
