"""M2 review probe: which OSC 7501 records does this Herdr accept as a pane's status?

usage (in a Herdr pane, then `hc get <pane>` and look at `status`):
  hc run --bg --name p1 python scripts/live/m2_osc_probe.py 'state=working' st
  hc run --bg --name p2 python scripts/live/m2_osc_probe.py 'state=working;app=kelir' st
Records are separated by '|'; the terminator is `st` (ESC backslash) or `bel`. Holds the pane 25 s.
Found with hc 0.1.21: only a bare `state=<word>` sets `status`; any extra key (app, id, msg,
title, progress, kind, agent) or a trailing `;` leaves it null.
"""
import sys
import time

term = {"st": "\x1b\\", "bel": "\x07"}[sys.argv[2]]
sys.stdout.write("".join("\x1b]7501;" + rec + term for rec in sys.argv[1].split("|")))
sys.stdout.flush()
time.sleep(25)
