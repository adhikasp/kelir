"""Summarise a winspy log for one scenario.

Usage: python -I winspy_report.py <spy.log> <tag>

<tag> is a string that appears in the command line of the scenario's root process (the pane's
`bash.exe -c ...`, the detached launcher, ...). Processes and windows are attributed to the scenario
when the root is in their parent chain. Prints one JSON object on the last line and a readable
block before it. Visible console/terminal windows are the FLASH evidence.
"""
import json, re, sys

PROC = re.compile(r"^(\S+) PROC (?:\* | {2})(\S+) pid=(\d+) ppid=(\d+) chain=(\S*) cmd=(.*)$")
WIN = re.compile(r'^(?:\*\* )?(\S+) WIN  (\S+) hwnd=(\S+) class=(.*?) vis=(\d) title="(.*?)" pid=(\d+) proc=(\S+) chain=(\S*)$')
INTEREST = {"conhost", "openconsole", "windowsterminal", "bash", "sleep", "kelir", "kelir-wip-snapshot", "cmd", "sh"}

log, tag = sys.argv[1], sys.argv[2]
procs, wins = {}, []
for line in open(log, encoding="utf-8", errors="replace"):
    line = line.rstrip("\n")
    m = PROC.match(line)
    if m:
        t, name, pid, ppid, chain, cmd = m.groups()
        procs[int(pid)] = dict(t=t, name=name, ppid=int(ppid), cmd=cmd)
        continue
    m = WIN.match(line)
    if m:
        t, src, hwnd, cls, vis, title, pid, proc, chain = m.groups()
        wins.append(dict(t=t, src=src, hwnd=hwnd, cls=cls, vis=vis == "1", title=title, pid=int(pid), proc=proc, chain=chain))

roots = [p for p, d in procs.items() if tag in d["cmd"] and d["name"].lower() != "winspy"]
mine = set(roots)
changed = True
while changed:
    changed = False
    for p, d in procs.items():
        if p not in mine and d["ppid"] in mine:
            mine.add(p); changed = True

def pids_in(chain):
    return {int(x) for x in re.findall(r"\((\d+)\)", chain)}

counts = {}
for p in mine:
    n = procs[p]["name"].lower()
    counts[n] = counts.get(n, 0) + 1

console_names = ("conhost", "openconsole", "windowsterminal")
cons = {n: counts.get(n, 0) for n in console_names}
flash = []
hidden = 0
for w in wins:
    if w["cls"] not in ("ConsoleWindowClass", "PseudoConsoleWindow", "CASCADIA_HOSTING_WINDOW_CLASS") and not w["vis"]:
        continue
    mine_w = bool(pids_in(w["chain"]) & mine)
    if w["vis"] and (w["cls"] in ("ConsoleWindowClass", "PseudoConsoleWindow", "CASCADIA_HOSTING_WINDOW_CLASS")
                     or any(k in w["title"].lower() for k in ("bash.exe", "kelir", "cmd.exe", "pwsh"))):
        flash.append(dict(w, attributed=mine_w))
    elif not w["vis"] and mine_w and w["src"] == "POLL":
        hidden += 1

# a visible terminal window's real source: the console client in the PseudoConsole window's chain
seen, sites = set(), []
for w in flash:
    key = (w["cls"], w["pid"])
    if key in seen:
        continue
    seen.add(key)
    p = procs.get(w["pid"], {})
    par = procs.get(p.get("ppid"), {})
    sites.append(dict(t=w["t"], cls=w["cls"], attributed=w["attributed"], pid=w["pid"], proc=w["proc"],
                      cmd=p.get("cmd", "")[:300], parent=par.get("name", ""), parent_cmd=par.get("cmd", "")[:300],
                      chain=w["chain"]))

out = dict(log=log, tag=tag, roots=roots, descendant_processes=len(mine),
           console_procs={k: v for k, v in cons.items()},
           by_name={k: v for k, v in sorted(counts.items()) if k in INTEREST},
           visible_windows_total=len(flash), visible_windows_attributed=sum(1 for w in flash if w["attributed"]),
           hidden_console_windows_attributed=hidden, flash_sites=sites)
print("roots:", roots, " descendants:", len(mine))
print("processes (scenario descendants):", out["by_name"])
print("console/terminal procs:", out["console_procs"])
print("visible console/terminal windows: total=%d attributed=%d; hidden console windows (attributed)=%d" % (
    out["visible_windows_total"], out["visible_windows_attributed"], hidden))
for s in sites:
    print("  FLASH", s["t"], s["cls"], "attributed" if s["attributed"] else "UNATTRIBUTED", "|", s["proc"], s["pid"], "|", s["cmd"], "| parent:", s["parent"], s["parent_cmd"][:160])
print(json.dumps(out))
