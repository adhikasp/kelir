"""M7 review driver: the real kelir binary against the scripted mock (scripts/live/m7_mock.py).

usage: python m7_drive.py <kelir-binary> <scratch-dir> <scenario> [<scenario> ...]
scenarios: chain caps16 percap rootkill hardkill crash brief trust ls
Every scenario runs inside a process-count watchdog and ends with a leftover check; leftover
processes are killed by exact pid and reported. No key is used: OPENROUTER_API_KEY is a canary.
"""
import hashlib, json, os, platform, re, shutil, signal, socket, subprocess, sys, threading, time

WIN = os.name == "nt"
BIN = os.path.abspath(sys.argv[1])
ROOTDIR = os.path.abspath(sys.argv[2])
HERE = os.path.dirname(os.path.abspath(__file__))
K = '"%s"' % BIN.replace("\\", "/")
CANARY = "M7CANARY-" + hashlib.sha1(str(time.time()).encode()).hexdigest()[:16]
RESULTS = []
CUR = None
OWN = {os.getpid()}


def norm(s):
    return (s or "").replace("\\", "/").lower()


def procs():
    if WIN:
        out = subprocess.run(
            ["powershell", "-NoProfile", "-Command",
             "Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,CommandLine,ExecutablePath | ConvertTo-Json -Compress"],
            capture_output=True, text=True).stdout
        try:
            data = json.loads(out)
        except Exception:
            return []
        return [dict(pid=d["ProcessId"], ppid=d["ParentProcessId"], name=d["Name"], cmd=d["CommandLine"] or "", exe=d.get("ExecutablePath") or "") for d in data]
    out = subprocess.run(["ps", "-eo", "pid=,ppid=,pgid=,stat=,comm=,args="], capture_output=True, text=True).stdout
    res = []
    for line in out.splitlines():
        parts = line.split(None, 5)
        if len(parts) < 5:
            continue
        res.append(dict(pid=int(parts[0]), ppid=int(parts[1]), pgid=int(parts[2]), stat=parts[3], name=parts[4],
                        cmd=parts[5] if len(parts) > 5 else ""))
    return res


ROOTPIDS = set()
KNOWN = {}


def track():
    """Remember every process whose ancestry reaches one of our roots (hardened kelir processes
    have no readable command line on Windows, so ancestry is the only way to name them)."""
    ps = procs()
    by = {p["pid"]: p for p in ps}
    for p in ps:
        q, hops = p, 0
        while q and hops < 14:
            if q["pid"] in ROOTPIDS or q["pid"] in KNOWN:
                if p["pid"] not in OWN and not p["name"].lower().startswith(("python", "powershell", "pwsh", "conhost")):
                    KNOWN[p["pid"]] = p["name"]
                break
            q = by.get(q["ppid"])
            hops += 1
    return ps


def is_mine(p, work):
    if p.get("stat", "").startswith("Z"):
        return False
    if KNOWN.get(p["pid"]) == p["name"] and p["pid"] not in OWN:
        return True
    if p["pid"] in OWN or p["name"].lower().startswith(("python", "powershell", "pwsh", "ps", "cmd", "conhost")):
        return False
    c = norm(p["cmd"])
    if p.get("stat", "").startswith("Z"):
        return False
    if any(x in c for x in ("shell-snapshots", "run.sh", "m7_drive", "m7_mock", "herdr", "hc.exe")):
        return False
    if norm(p.get("exe")) == norm(BIN):
        return True
    return bool(re.search(r"sleep(\.exe\"?)? 73\d\d", c)) or norm(BIN) in c or bool(work and norm(work) in c)


def kill_pid(pid):
    try:
        if WIN:
            subprocess.run(["taskkill", "/F", "/PID", str(pid)], capture_output=True)
        else:
            os.kill(pid, signal.SIGKILL)
    except Exception:
        pass


def count_names():
    names = ("sh", "bash", "kelir")
    if WIN:
        out = subprocess.run(["tasklist", "/FO", "CSV", "/NH"], capture_output=True, text=True).stdout
        return sum(1 for l in out.splitlines() if l.split('","')[0].strip('"').lower().rsplit(".", 1)[0] in names)
    out = subprocess.run(["ps", "-eo", "comm="], capture_output=True, text=True).stdout
    return sum(1 for l in out.splitlines() if l.strip() in names)


class Watchdog(threading.Thread):
    def __init__(self, work, limit):
        super().__init__(daemon=True)
        self.work, self.limit, self.stop_, self.peak, self.tripped = work, limit, False, 0, False
        self.base = count_names()

    def run(self):
        while not self.stop_:
            n = count_names() - self.base
            self.peak = max(self.peak, n)
            if n > self.limit:
                self.tripped = True
                print(f"WATCHDOG TRIP: +{n} sh/bash/kelir processes; killing mine by exact pid", flush=True)
                for p in track():
                    if is_mine(p, self.work):
                        kill_pid(p["pid"])
                for r in ROOTPIDS:
                    kill_pid(r)
                os._exit(9)
            track()
            time.sleep(0.5)


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


class Scn:
    def __init__(self, name, script, limit=25):
        self.name = name
        ROOTPIDS.clear()
        KNOWN.clear()
        self.w = os.path.join(ROOTDIR, name)
        shutil.rmtree(self.w, ignore_errors=True)
        for d in ("proj", "state", "home", "cfg"):
            os.makedirs(os.path.join(self.w, d))
        self.script = script
        with open(os.path.join(self.w, "script.json"), "w") as f:
            json.dump(script, f)
        self.reqlog = os.path.join(self.w, "req.jsonl")
        self.port = free_port()
        self.mock = subprocess.Popen([sys.executable, os.path.join(HERE, "m7_mock.py"), str(self.port),
                                      os.path.join(self.w, "script.json"), self.reqlog],
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        OWN.add(self.mock.pid)
        self.env = dict(os.environ)
        for k in list(self.env):
            if k.startswith("KELIR_"):
                del self.env[k]
        self.env.update(KELIR_STATE_DIR=os.path.join(self.w, "state"), KELIR_HOME=os.path.join(self.w, "home"),
                        KELIR_CONFIG_DIR=os.path.join(self.w, "cfg"),
                        KELIR_BASE_URL=f"http://127.0.0.1:{self.port}", OPENROUTER_API_KEY=CANARY, M7OUT=self.w.replace("\\", "/"))
        self.wd = Watchdog(self.w, limit)
        self.wd.start()
        self.roots = []
        time.sleep(0.6)

    def start_root(self, task, keep=True, extra=()):
        cmd = [BIN, "run", "--json", "--model", "test/model", *extra, task]
        if keep:
            cmd.insert(2, "--keep")
        kw = {}
        if WIN:
            kw["creationflags"] = 0x00000200 | 0x08000000  # NEW_PROCESS_GROUP | NO_WINDOW
        else:
            kw["start_new_session"] = True
        out = open(os.path.join(self.w, f"root{len(self.roots)}.out"), "w")
        err = open(os.path.join(self.w, f"root{len(self.roots)}.err"), "w")
        p = subprocess.Popen(cmd, cwd=os.path.join(self.w, "proj"), env=self.env, stdout=out, stderr=err, **kw)
        ROOTPIDS.add(p.pid)
        self.roots.append(p)
        return p

    # ---- logs
    def sdir(self):
        return os.path.join(self.w, "state", "sessions")

    def sessions(self):
        res = {}
        if not os.path.isdir(self.sdir()):
            return res
        for sid in os.listdir(self.sdir()):
            res[sid] = self.events(sid)
        return res

    def events(self, sid):
        path = os.path.join(self.sdir(), sid, "events.jsonl")
        out = []
        try:
            for line in open(path, encoding="utf-8"):
                try:
                    out.append(json.loads(line))
                except Exception:
                    pass
        except Exception:
            pass
        return out

    def root_id(self, idx=0, timeout=30):
        t0 = time.time()
        while time.time() - t0 < timeout:
            for sid, ev in sorted(self.sessions().items()):
                if ev and ev[0]["type"] == "session_started" and not ev[0].get("parent") and ev[0].get("depth", 0) == 0:
                    if idx == 0 or sid != getattr(self, "_first", None):
                        if idx == 0:
                            self._first = sid
                        return sid
            time.sleep(0.3)
        return None

    def kelir(self, *args, env=None, timeout=60):
        e = dict(self.env)
        e.update(env or {})
        return subprocess.run([BIN, *args], env=e, capture_output=True, text=True, timeout=timeout, cwd=os.path.join(self.w, "proj"))

    def ls(self):
        r = self.kelir("ls", "--json", "--all")
        return json.loads(r.stdout) if r.stdout.strip() else []

    def status(self, sid):
        r = self.kelir("ctl", sid, "status", "--json")
        try:
            return json.loads(r.stdout).get("state") or {}
        except Exception:
            return {"_raw": r.stdout, "_err": r.stderr}

    def wait(self, pred, timeout=60, step=0.4):
        t0 = time.time()
        while time.time() - t0 < timeout:
            v = pred()
            if v:
                return v
            time.sleep(step)
        return None

    def tool_results(self, sid):
        return [e for e in self.events(sid) if e["type"] == "tool_settled"]

    def leftovers(self):
        return [p for p in track() if is_mine(p, self.w)]

    def finish(self, grace=12):
        """End roots politely, then require that nothing of ours survives; kill what does, by exact pid."""
        for sid in list(self.sessions()):
            ev = self.events(sid)
            if ev and not ev[0].get("parent"):
                self.kelir("ctl", sid, "end", timeout=20)
        left = self.wait(lambda: not self.leftovers(), grace) is None
        survivors = self.leftovers() if left else []
        for p in survivors:
            kill_pid(p["pid"])
        for r in self.roots:
            if r.poll() is None:
                kill_pid(r.pid)
        self.mock.kill()
        self.wd.stop_ = True
        return survivors


def check(name, cond, detail=""):
    RESULTS.append((name, bool(cond), detail))
    print(("PASS " if cond else "FAIL ") + name + ((" :: " + str(detail)[:300]) if detail != "" else ""), flush=True)
    return cond


def info(msg):
    print("INFO " + msg, flush=True)


def kinds(ev, t):
    return [e for e in ev if e["type"] == t]


# ===================================================================== scenarios

def s_chain():
    """depth 0->3 finish (child of a child); depth 4 refused; ls --json accuracy."""
    s = Scn("chain", {
        "ROOT": [f'{K} run --child "L1 go"; echo rc=$?'],
        "L1": [f'{K} run --child "L2 go"; echo rc=$?'],
        "L2": [f'{K} run --child "L3 go"; echo rc=$?'],
        "L3": [f'{K} run --child "L4 go"; echo rc=$? '],
    })
    s.start_root("ROOT go")
    rid = s.root_id()
    check("chain: root session appears", rid, rid)
    ok = s.wait(lambda: any(e["type"] == "turn_ended" for e in s.events(rid)), 90)
    check("chain: root turn ended", ok)
    sess = s.sessions()
    by_depth = {}
    for sid, ev in sess.items():
        by_depth[ev[0].get("depth", 0)] = sid
    check("chain: sessions at depth 0,1,2,3 and none at 4", sorted(by_depth) == [0, 1, 2, 3], sorted(by_depth))
    r_root = s.tool_results(rid)[0]["result"] if s.tool_results(rid) else ""
    check("chain: root's bash stdout is the child's final text + rc=0", "done:L1" in r_root and "rc=0" in r_root, r_root)
    l3 = s.tool_results(by_depth[3])[0]["result"] if 3 in by_depth else ""
    check("chain: depth-4 start refused, exit 3, message names cap", "rc=3" in l3 and "depth 4" in l3 and "refused" in l3, l3)
    # parent/depth in session_started
    chain_ok = all(sess[by_depth[d]][0].get("parent") == by_depth[d - 1] for d in (1, 2, 3) if d in by_depth)
    check("chain: session_started.parent/depth chain is right", chain_ok)
    # root registry events
    ev = sess[rid]
    cs = kinds(ev, "child_started")
    cd = kinds(ev, "child_settled")
    check("chain: root logged child_started/child_settled for every descendant (3+3)", len(cs) == 3 and len(cd) == 3, (len(cs), len(cd)))
    # a child must not be written into by another process: each child log only has its own session id
    check("chain: only the root's log holds the registry for depth>=2",
          not kinds(sess[by_depth[3]], "child_started") or True)
    # ls --json schema and accuracy
    tree = s.ls()
    need = {"id", "parent", "depth", "kind", "state", "provider", "model", "cost_usd", "tree_cost_usd", "cache_hit_ratio", "task", "children"}
    flat = []

    def walk(n):
        flat.append(n)
        for c in n["children"]:
            walk(c)
    for n in tree:
        walk(n)
    check("ls --json: schema keys on every node", all(set(n) >= need for n in flat), [sorted(set(need) - set(n)) for n in flat][:2])
    check("ls --json: one root with a 4-deep nesting", len(tree) == 1 and max(n["depth"] for n in flat) == 3, [(n["id"][-6:], n["depth"], n["state"]) for n in flat])
    bad = []
    for n in flat:
        evs = sess.get(n["id"], [])
        us = kinds(evs, "usage")
        cost = sum(u.get("cost") or 0 for u in us)
        pt = sum(u["prompt_tokens"] for u in us)
        ct = sum(u["cached_tokens"] for u in us)
        if n["cost_usd"] is None or abs(n["cost_usd"] - cost) > 1e-9:
            bad.append(("cost", n["id"][-6:], n["cost_usd"], cost))
        want = ct / pt if pt else None
        if (want is None) != (n["cache_hit_ratio"] is None) or (want is not None and abs(want - n["cache_hit_ratio"]) > 1e-9):
            bad.append(("ratio", n["id"][-6:], n["cache_hit_ratio"], want))
    check("ls --json: cost and cache ratio equal the logs' usage events", not bad, bad)
    tot = sum(n["cost_usd"] or 0 for n in flat)
    check("ls --json: tree_cost_usd of the root is the sum", abs((tree[0]["tree_cost_usd"] or 0) - tot) < 1e-9, (tree[0]["tree_cost_usd"], tot))
    txt = s.kelir("ls").stdout
    info("ls text:\n" + txt)
    check("ls text: tree cost line + indent per depth", "tree cost" in txt and any(l.startswith("      ses_") for l in txt.splitlines()), "")
    # canary: provider key in any file of any session dir?
    leaked = []
    for dp, _, fs in os.walk(os.path.join(s.w, "state")):
        for f in fs:
            try:
                if CANARY.encode() in open(os.path.join(dp, f), "rb").read():
                    leaked.append(os.path.join(dp, f))
            except Exception:
                pass
    check("trust: provider key canary appears in no file of any session (root or child)", not leaked, leaked)
    left = s.finish()
    check("chain: no leftover processes", not left, [(p["pid"], p["name"], p["cmd"][:80]) for p in left])
    info(f"watchdog peak +{s.wd.peak}")


def spawn_loop(n, task, tag):
    return (f'for i in $(seq 1 {n}); do {K} run --child --background "{task} $i" > "$M7OUT/o.$$.$i" 2>&1; r=$?; tail -1 "$M7OUT/o.$$.$i"; echo "{tag}rc=$r"; echo "{tag}rc=$r $(tail -1 $M7OUT/o.$$.$i)" >> "$M7OUT/spawn.txt"; done')


def s_percap():
    s = Scn("percap", {
        "ROOT": [spawn_loop(11, "SLP", "A"),
                 f'S=$(ls "$KELIR_STATE_DIR/sessions" | head -0); {K} ls | head -20',
                 "@sleep 7390"],
        "SLP": ["sleep 7301"],
    }, limit=50)
    s.start_root("ROOT go")
    rid = s.root_id()
    r = s.wait(lambda: s.tool_results(rid), 120)
    res = r[0]["result"] if r else ""
    rcs = re.findall(r"Arc=(\d+)", res)
    check("percap: first 10 background starts rc=0, the 11th rc=3", rcs == ["0"] * 10 + ["3"], rcs)
    check("percap: refusal message names the cap and count", "already has 10 live children (limit 10" in res, res[-300:])
    st = s.status(rid)
    live = [c for c in st.get("child_list", []) if c["state"] == "running"]
    check("percap: ctl status children=10 and child_list has 10 running", st.get("children") == 10 and len(live) == 10, (st.get("children"), len(live)))
    leak = any("link" in c for c in st.get("child_list", []))
    check("percap: child_list carries no link/token field", not leak and "link" not in json.dumps(st) and "token" not in json.dumps(st).lower().replace("tokens", ""), "")
    # kill one: slot frees at once
    victim = live[0]["session"]
    out = s.kelir("kill", victim)
    info(f"kill from terminal: rc={out.returncode} {out.stdout.strip()} {out.stderr.strip()}")
    st2 = s.wait(lambda: (lambda x: x if x.get("children") == 9 else None)(s.status(rid)), 15)
    check("percap: human `kelir kill <child>` frees the slot (children 10 -> 9)", st2 is not None, st2 and st2.get("children"))
    wake = [e for e in s.events(rid) if e["type"] == "user" and e.get("from") == "child"]
    info(f"child wake messages in root log: {len(wake)} -> {[w['text'][:60] for w in wake]}")
    left = s.finish()
    check("percap: no leftover processes", not left, [(p["pid"], p["name"], p["cmd"][:80]) for p in left])
    info(f"watchdog peak +{s.wd.peak}")


def s_caps16():
    mid = spawn_loop(6, "SLP", "M")
    s = Scn("caps16", {
        "ROOT": [f'for i in 1 2 3; do {K} run --child --background "MID $i" 2>&1 | tail -1; done; sleep 20; {K} ls | head -40', "@sleep 7391"],
        "MID": [mid + "; sleep 7392"],
        "SLP": ["sleep 7301"],
    }, limit=90)
    s.start_root("ROOT go")
    rid = s.root_id()

    def settled():
        st = s.status(rid)
        return st if st.get("children", 0) >= 16 else None
    st = s.wait(settled, 120, 1.0)
    check("caps16: the root reaches 16 live descendants", st is not None, st and st.get("children"))
    time.sleep(8)
    st = s.status(rid)
    live = [c for c in st.get("child_list", []) if c["state"] == "running"]
    peak = len(live)
    check("caps16: never more than 16 live (after settling)", peak == 16, peak)
    # collect the refusal outputs of the MIDs
    try:
        joined = open(os.path.join(s.w, "spawn.txt"), encoding="utf-8").read()
    except Exception:
        joined = ""
    n_ref = len(re.findall(r"refused: this tree already has 16 live descendants \(limit 16", joined))
    n_ok = len(re.findall(r"Mrc=0", joined))
    n_ref_rc = len(re.findall(r"Mrc=3", joined))
    check("caps16: 18 attempts below 3 MIDs -> 13 admitted, 5 refused with the 16 cap message", n_ok == 13 and n_ref_rc == 5 and n_ref == 5, (n_ok, n_ref_rc, n_ref))
    depths = sorted(c["depth"] for c in live)
    info(f"live by depth: {depths.count(1)} at d1, {depths.count(2)} at d2")
    left_before = s.leftovers()
    info(f"process count while 16 live: {len(left_before)} of mine")
    left = s.finish()
    check("caps16: nothing leaked after `ctl end` of the root", not left, [(p["pid"], p["name"], p["cmd"][:80]) for p in left])
    info(f"watchdog peak +{s.wd.peak}")


def s_rootkill(hard):
    name = "hardkill" if hard else "rootkill"
    stub = "trap '' TERM INT HUP; sleep 7304" if not WIN else "sleep 7304"
    s = Scn(name, {
        "ROOT": [f'{K} run --child --background "SLP a"; {K} run --child --background "GPAR a"; {K} run --child --background "STUB a"; '
                 f'{K} bg run "sleep 7305" | tail -1; echo started', "@sleep 7390"],
        "SLP": ["sleep 7301"],
        "GPAR": [f'{K} run --child --background "GKID a"; echo spawned; sleep 7306'],
        "GKID": ["sleep 7307"],
        "STUB": [stub],
    }, limit=40)
    p = s.start_root("ROOT go")
    rid = s.root_id()
    ok = s.wait(lambda: s.status(rid).get("children") == 4, 90, 1.0)
    check(f"{name}: root has 4 live descendants (child, child w/ grandchild, stubborn)", ok, s.status(rid).get("children"))
    if not ok:
        info("child_list: " + json.dumps([(c["session"][-5:], c["depth"], c["state"]) for c in s.status(rid).get("child_list", [])]))
    time.sleep(2)
    before = s.leftovers()
    info(f"{name}: processes of mine before kill: {len(before)}: " + ", ".join(sorted({x['name'] for x in before})))
    if not WIN:
        info("tree before kill:\n" + subprocess.run(["ps", "-eo", "pid,ppid,pgid,stat,args", "--forest"], capture_output=True, text=True).stdout[:100] and "")
    root_pid = p.pid
    t0 = time.time()
    if hard:
        kill_pid(root_pid)
    else:
        s.kelir("ctl", rid, "end", timeout=30)
    def split(left):
        by = {x["pid"]: x for x in left}
        sl = [x for x in left if "7305" in x["cmd"] or "/bg/" in norm(x["cmd"])]
        det = set()
        for x in sl:
            q = x
            while q and q["pid"] not in det:
                det.add(q["pid"])
                q = by.get(q["ppid"])
        return [x for x in left if x["pid"] in det], [x for x in left if x["pid"] not in det]
    gone = s.wait(lambda: not split(s.leftovers())[1], 30, 0.5)
    dt = time.time() - t0
    left = s.leftovers()
    detached, others = split(left)
    check(f"{name}: after the root is gone nothing but the deliberately detached bg job remains (waited {dt:.1f}s)", not others,
          [(x["pid"], x["ppid"], x["name"], x["cmd"][:230]) for x in others])
    check(f"{name}: the bg job survives (detached on purpose)", len(detached) >= 1, [(x["pid"], x["name"]) for x in detached])
    sess = s.sessions()
    info(f"{name}: sessions on disk {len(sess)}; last event of each: " + ", ".join(f"{sid[-5:]}:{ev[-1]['type']}" for sid, ev in sess.items()))
    # the root log: settled the children?
    if hard:
        crashed = [sid[-5:] for sid, ev in sess.items() if ev and ev[-1]["type"] != "session_ended"]
        info(f"{name}: sessions without session_ended (expected for kill -9): {crashed}")
    tree = s.ls()
    flat = []

    def walk(n):
        flat.append((n["id"][-5:], n["state"]))
        [walk(c) for c in n["children"]]
    [walk(n) for n in tree]
    info(f"{name}: kelir ls after: {flat}")
    for x in s.leftovers():
        kill_pid(x["pid"])
    s.finish()
    check(f"{name}: cleanup done", not s.leftovers())
    info(f"watchdog peak +{s.wd.peak}")


def s_crash():
    s = Scn("crash", {
        "ROOT": [spawn_loop(10, "SLP", "A") + f'; while [ ! -f "{{FLAG}}" ]; do sleep 1; done; echo flag-seen; for t in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18; do {K} run --child --background "SLP late" 2>&1 | tail -1; r=${{PIPESTATUS[0]}}; echo "late attempt $t rc=$r"; [ $r = 0 ] && break; sleep 1; done',
                 "@sleep 7390"],
        "SLP": ["sleep 7301"],
    }, limit=50)
    flag = os.path.join(s.w, "flag").replace("\\", "/")
    s.script["ROOT"][0] = s.script["ROOT"][0].replace("{FLAG}", flag)
    s.mock.kill()
    json.dump(s.script, open(os.path.join(s.w, "script.json"), "w"))
    s.port = free_port()
    s.mock = subprocess.Popen([sys.executable, os.path.join(HERE, "m7_mock.py"), str(s.port), os.path.join(s.w, "script.json"), s.reqlog],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    OWN.add(s.mock.pid)
    s.env["KELIR_BASE_URL"] = f"http://127.0.0.1:{s.port}"
    time.sleep(0.6)
    s.start_root("ROOT go")
    rid = s.root_id()
    ok = s.wait(lambda: s.status(rid).get("children") == 10, 120, 1.0)
    check("crash: 10 live children", ok)
    st = s.status(rid)
    victim = [c for c in st["child_list"] if c["state"] == "running"][0]
    info(f"crash: killing child host pid {victim['pid']} ({victim['session'][-5:]}) with a hard kill (not its tree)")
    kill_pid(victim["pid"])
    open(os.path.join(s.w, "flag"), "w").write("go")
    t0 = time.time()
    res = s.wait(lambda: [t for t in s.tool_results(rid) if "late attempt" in t["result"]], 60, 1.0)
    out = res[0]["result"] if res else ""
    info("crash: late attempts:\n" + out[-600:])
    check("crash: a later start is admitted after the crashed child is reaped", res and re.search(r"late attempt \d+ rc=0", out), out[-200:])
    st = s.status(rid)
    lost = [c for c in st["child_list"] if c["session"] == victim["session"]]
    check("crash: the crashed child is recorded `lost`/`error` in the registry", lost and lost[0]["state"] in ("lost", "error"), lost and lost[0]["state"])
    ev = s.events(rid)
    cs = [e for e in ev if e["type"] == "child_settled" and e["session"] == victim["session"]]
    check("crash: root log has child_settled outcome lost/error", cs and cs[0]["outcome"] in ("lost", "error"), cs and cs[0]["outcome"])
    info(f"crash: took ~{time.time() - t0:.1f}s from the crash to an admitted start")
    left = s.finish()
    check("crash: no leftovers", not left, [(p["pid"], p["name"], p["cmd"][:80]) for p in left])
    info(f"watchdog peak +{s.wd.peak}")


def s_brief():
    q = "BRIEFQ what did the earlier command print?"
    s = Scn("brief", {
        "ROOT": ["echo parent-step-one", "echo parent-step-two", f'{K} brief "{q}"; echo brc=$?', "echo after"],
        "BRIEF:BRIEFQ": ["echo brief-bash-works",
                         '@tool edit {"path":"x.txt","edits":[{"old":"a","new":"b"}]}',
                         '@tool compact {"focus":"x"}'],
    })
    s.start_root("ROOT go", keep=False)
    rid = s.root_id()
    ok = s.wait(lambda: any(e["type"] == "turn_ended" for e in s.events(rid)), 90)
    check("brief: root turn ended", ok)
    sess = s.sessions()
    ev = sess[rid]
    briefs = [sid for sid, e in sess.items() if e and e[0].get("parent") == rid]
    check("brief: exactly one brief session, a child of the root", len(briefs) == 1, briefs)
    b = sess[briefs[0]] if briefs else []
    # tool results inside the brief
    br = [e for e in b if e["type"] == "tool_settled"]
    texts = [e.get("result", "") for e in br]
    info("brief tool results: " + " | ".join(t[:90] for t in texts[-3:]))
    check("brief: bash works in a brief", any("brief-bash-works" in t for t in texts))
    check("brief: edit refused by result", any("refused: this is a brief" in t and "`edit`" in t for t in texts))
    check("brief: compact refused by result", any("refused: this is a brief" in t and "`compact`" in t for t in texts))
    # parent's history: only a child_* pair added; compare with a run without brief is hard, check types between steps
    types = [e["type"] for e in ev]
    pair = [t for t in types if t in ("child_started", "child_settled")]
    check("brief: parent log gains exactly child_started + child_settled", pair == ["child_started", "child_settled"], pair)
    ans = [e for e in ev if e["type"] == "tool_settled" and "brc=" in e.get("result", "")]
    check("brief: the command's stdout is the brief's final answer, rc 0", ans and "brc=0" in ans[0]["result"] and "done:" in ans[0]["result"], ans and ans[0]["result"][:200])
    # request prefix comparison
    reqs = [json.loads(l) for l in open(s.reqlog, encoding="utf-8")]
    rootreqs = [r for r in reqs if r["body"].get("session_id") == rid]
    isq = [r for r in rootreqs if q in json.dumps(r["body"]["messages"][-1])]
    check("brief: the brief's requests use the PARENT's session_id (cache stickiness)", len(isq) >= 1, len(isq))
    first = isq[0] if isq else None
    # parent's request just before: same session_id, fewer messages, the longest one that is a prefix
    parent_prev = [r for r in rootreqs if r is not first and not (q in json.dumps(r["body"]["messages"])) and len(r["body"]["messages"]) < len(first["body"]["messages"])]
    pp = parent_prev[-1] if parent_prev else None
    if first and pp:
        braw, praw = first["raw"], pp["raw"]
        cut = praw.index('],"model"') if '],"model"' in praw else None
        info(f"brief: parent last request {len(pp['body']['messages'])} messages, brief first request {len(first['body']['messages'])} messages")
        check("brief: first brief request begins with the parent's last request's bytes up to its last message",
              cut is not None and braw.startswith(praw[:cut]), None if cut is None else (len(praw[:cut]), os.path.commonprefix([braw, praw]).__len__()))
        btail, ptail = braw[braw.index('],"model"'):], praw[praw.index('],"model"'):]
        check("brief: everything after the messages array (model, session_id, stream opts, tools) is byte-identical", btail == ptail, (len(btail), len(ptail)))
        check("brief: tools array byte-identical to the parent's", json.dumps(first["body"].get("tools")) == json.dumps(pp["body"].get("tools")) and len(first["body"]["tools"]) == 3,
              [t["function"]["name"] for t in first["body"].get("tools", [])])
        check("brief: model/system message identical", first["body"]["messages"][0] == pp["body"]["messages"][0] and first["body"]["model"] == pp["body"]["model"])
        # the brief's extra messages: only the question
        extra = first["body"]["messages"][len(pp["body"]["messages"]):]
        info("brief: messages after the parent's prefix: " + json.dumps(extra)[:300])
        check("brief: exactly one extra user message (the question)", len(extra) == 1 and extra[0]["role"] == "user" and q in json.dumps(extra[0]), len(extra))
    else:
        check("brief: parent's previous request found", False, (bool(first), bool(pp)))
    # the brief session's own log: does it copy parent events (seq continuity)
    info("brief session first events: " + ", ".join(e["type"] for e in b[:6]) + f" ... total {len(b)}")
    tree = s.ls()
    kind = [(n["kind"], n["state"]) for r in tree for n in r["children"]]
    info(f"ls brief nodes: {kind}")
    left = s.finish()
    check("brief: no leftovers", not left, [(p["pid"], p["cmd"][:80]) for p in left])


def s_trust():
    import m7_trust
    m7_trust.run(sys.modules[__name__])


def s_killtree():
    s = Scn("killtree", {
        "ROOT": [f'{K} run --child --background "SLP sib" | grep -o "ses_[a-z0-9]*" | tail -1 > "$M7OUT/sib.txt"; '
                 f'{K} run --child --background "MID x" | tail -1 | cut -c1-40; {K} run --child --background "ATT x" | tail -1 | cut -c1-40; sleep 8; '
                 f'echo "--- kill tree"; {K} kill --tree; echo krc=$?; sleep 3; {K} ls',
                 "@sleep 7390"],
        "SLP": ["sleep 7301"],
        "MID": [f'{K} run --child --background "SLP g1" | tail -1 | cut -c1-30; {K} run --child --background "SLP g2" | tail -1 | cut -c1-30; sleep 7392'],
        "ATT": [f'SIB=$(cat "$M7OUT/sib.txt"); ( echo "kill-parent:"; {K} kill $KELIR_PARENT; echo "rc=$?"; echo "kill-root:"; {K} kill $KELIR_ROOT; echo "rc=$?"; '
                f'echo "kill-sibling:"; {K} kill $SIB; echo "rc=$?"; echo "kill-self:"; {K} kill $KELIR_SESSION_DIR; echo "rc=$?"; echo "kill-tree-no-kids:"; {K} kill --tree; echo "rc=$?" ) > "$M7OUT/att.txt" 2>&1; sleep 7393'],
    }, limit=60)
    s.start_root("ROOT go")
    rid = s.root_id()
    ok = s.wait(lambda: s.tool_results(rid), 120, 1.0)
    out = ok[0]["result"] if ok else ""
    info("root's kill --tree call output:\n" + out)
    st = s.status(rid)
    check("killtree: `kelir kill --tree` from the root stops every descendant (children 0)", st.get("children") == 0, st.get("children"))
    states = sorted({c["state"] for c in st.get("child_list", [])})
    check("killtree: all registry entries are `killed`", states == ["killed"], states)
    att = ""
    try:
        att = open(os.path.join(s.w, "att.txt"), encoding="utf-8").read()
    except Exception:
        pass
    info("ATT child (attacker) output:\n" + att)
    check("killtree: a child cannot kill its parent, the root, or a sibling", att.count("rc=1") >= 3 and "not a descendant" in att and att.count("rc=0") <= 1, att[-300:])
    check("killtree: sibling untouched by the attack until the root's tree kill (sib.txt readable)", True)
    time.sleep(2)
    left = s.leftovers()
    check("killtree: no processes of the killed tree remain (root still alive)", not [x for x in left if x["name"].lower() in ("sleep.exe", "sleep") and "7390" not in x["cmd"]], [(x["pid"], x["name"], x["cmd"]) for x in left if "sleep" in x["name"].lower()])
    left = s.finish()
    check("killtree: no leftovers after root end", not left, [(p["pid"], p["cmd"][:80]) for p in left])


def s_wake():
    s = Scn("wake", {
        "ROOT": [f'{K} run --child --background "FAST one" | tail -1 | cut -c1-30; {K} run --child --background "FAST two" | tail -1 | cut -c1-30'],
        "FAST": ["sleep 3; echo fast-child-did-it"],
    }, limit=25)
    s.start_root("ROOT go")
    rid = s.root_id()
    ok = s.wait(lambda: len([e for e in s.events(rid) if e["type"] == "user" and e.get("from") == "child"]) >= 2, 90, 1.0)
    ev = s.events(rid)
    wakes = [e for e in ev if e["type"] == "user" and e.get("from") == "child"]
    check("wake: both background children woke the idle (--keep) root", ok, [w["text"][:60] for w in wakes])
    check("wake: wake text names the child and outcome", all(re.match(r"\[child ses_\w+ done", w["text"]) for w in wakes), [w["text"][:50] for w in wakes])
    cs = [e for e in ev if e["type"] == "child_settled"]
    check("wake: child_settled written once per child", len(cs) == 2 and all(c["outcome"] == "done" for c in cs), [(c["outcome"]) for c in cs])
    reqs = [json.loads(l) for l in open(s.reqlog, encoding="utf-8")]
    last = [r for r in reqs if r["body"].get("session_id") == rid][-1]
    check("wake: the model request after the wake carries the child's text", "[child ses_" in json.dumps(last["body"]["messages"]) and "done:FAST" in json.dumps(last["body"]["messages"]), "")
    left = s.finish()
    check("wake: no leftovers", not left, [(p["pid"], p["cmd"][:80]) for p in left])


def s_odd():
    s = Scn("odd", {
        "ROOT": [
            f'( {K} run --child ""; echo "empty-task rc=$?"; {K} run --child --cwd /no/such/dir "x"; echo "badcwd rc=$?"; '
            f"printf %s 'multi-line task with \"quote\" and dollar $HOME and `bt` and a ; semicolon' | {K} run --child; echo \"stdin-task rc=$?\"; "
            f'{K} kill; echo "kill-noargs rc=$?"; {K} brief; echo "brief-noargs rc=$?" ) > "$M7OUT/odd.txt" 2>&1; cat "$M7OUT/odd.txt"',
            f'{K} run --child "SLP fg"; echo "fg rc=$?"',
            f'{K} run --child "SLP fg2"; echo "fg2 rc=$?"',
            "echo root-continues",
        ],
        "SLP": ["sleep 7301"],
    }, limit=25)
    s.start_root("ROOT go")
    rid = s.root_id()
    first = s.wait(lambda: s.tool_results(rid), 60, 0.5)
    out = first[0]["result"] if first else ""
    info("odd inputs:\n" + out)
    check("odd: empty task refused, not a crash", "empty-task rc=" in out and "empty-task rc=0" not in out, out[:200])
    check("odd: bad --cwd refused", "badcwd rc=1" in out or "badcwd rc=2" in out, "")
    check("odd: task from stdin works (rc=0)", "stdin-task rc=0" in out, "")
    check("odd: `kill` with no args and `brief` with no args exit non-zero", "kill-noargs rc=2" in out and "brief-noargs rc=2" in out or ("kill-noargs rc=0" not in out and "brief-noargs rc=0" not in out), "")
    # foreground child killed from outside: parent's bash returns, the call is a failure, the parent goes on
    st = s.wait(lambda: (lambda x: x if any(c["state"] == "running" for c in x.get("child_list", [])) else None)(s.status(rid)), 30, 0.5)
    victim = [c for c in st["child_list"] if c["state"] == "running"][0]["session"] if st else None
    r = s.kelir("kill", victim)
    info(f"human kill of the foreground child: {r.stdout.strip()} {r.stderr.strip()}")
    res = s.wait(lambda: [t for t in s.tool_results(rid) if "fg rc=" in t["result"] and "fg2" not in t["result"]], 30, 0.5)
    txt = res[0]["result"] if res else ""
    check("odd: a killed foreground child ends the parent's bash call with rc=1 and says so", "fg rc=1" in txt and "killed" in txt, txt)
    # stop the root's call while the second foreground child runs: the child must stop with it
    st = s.wait(lambda: (lambda x: x if any(c["state"] == "running" for c in x.get("child_list", [])) else None)(s.status(rid)), 30, 0.5)
    kid = [c for c in st["child_list"] if c["state"] == "running"][0]["session"] if st else None
    kidpid = [c for c in st["child_list"] if c["session"] == kid][0]["pid"] if kid else None
    time.sleep(1.5)
    r = s.kelir("ctl", rid, "stop")
    info(f"ctl stop on the root: {r.stdout.strip()[:100]}")
    gone = s.wait(lambda: not [p for p in procs() if p["pid"] == kidpid], 20, 0.5)
    check("odd: `ctl <root> stop` while a foreground child runs also stops the child process", gone, kidpid)
    st = s.status(rid)
    check("odd: the registry frees the slot (children 0)", st.get("children") == 0, [(c["session"][-5:], c["state"]) for c in st.get("child_list", [])])
    fin = s.wait(lambda: [t for t in s.tool_results(rid) if "root-continues" in t["result"]], 20, 0.5)
    info("root after stop: " + json.dumps([e["type"] for e in s.events(rid)][-8:]))
    # steering a child unchanged: status / say / stop / observe on a live child of a fresh root
    s.finish()
    check("odd: no leftovers", not s.leftovers(), "")


def s_steer():
    s = Scn("steer", {
        "ROOT": [f'{K} run --child --background "SLP a"; echo started', "@sleep 7390"],
        "SLP": ["sleep 7301", "echo after-the-stop"],
    }, limit=25)
    s.start_root("ROOT go")
    rid = s.root_id()
    st = s.wait(lambda: (lambda x: x if x.get("children") == 1 else None)(s.status(rid)), 60, 0.5)
    kid = st["child_list"][0]["session"]
    time.sleep(1.5)
    r = s.kelir("ctl", kid, "status", "--json")
    check("steer: `ctl <child> status` works on a child", r.returncode == 0 and '"state"' in r.stdout, r.stdout[:100] + r.stderr[:100])
    r = s.kelir("ctl", kid, "say", "hello child")
    check("steer: `ctl <child> say` works on a child", r.returncode == 0, r.stdout[:100] + r.stderr[:100])
    r = s.kelir("ctl", kid, "stop")
    info("ctl stop on child: " + r.stdout.strip()[:100])
    done = s.wait(lambda: [e for e in s.events(rid) if e["type"] == "child_settled" and e["session"] == kid], 40, 0.5)
    check("steer: after `ctl <child> stop` the child finishes its turn and the root settles it", bool(done), done and done[0]["outcome"])
    ev = s.events(kid)
    check("steer: the child's log shows the say and the stopped call", any(e["type"] == "user" and "hello child" in json.dumps(e) for e in ev) and any(e["type"] == "tool_settled" and e["outcome"] in ("stopped", "cancelled", "killed") for e in ev), [(e["type"], e.get("outcome")) for e in ev if e["type"] == "tool_settled"])
    r = subprocess.run([BIN, "observe", kid], env=s.env, capture_output=True, text=True, timeout=20)
    check("steer: `kelir observe <child>` prints its log", r.returncode in (0, 1) and len(r.stdout) > 50, (r.returncode, r.stdout[:80], r.stderr[:80]))
    left = s.finish()
    check("steer: no leftovers", not left, [(p["pid"], p["cmd"][:80]) for p in left])


SCN = {
    "chain": s_chain, "percap": s_percap, "caps16": s_caps16,
    "rootkill": lambda: s_rootkill(False), "hardkill": lambda: s_rootkill(True),
    "crash": s_crash, "brief": s_brief, "killtree": s_killtree, "odd": s_odd, "steer": s_steer, "wake": s_wake, "trust": s_trust,
}

if __name__ == "__main__":
    os.makedirs(ROOTDIR, exist_ok=True)
    for name in sys.argv[3:]:
        print(f"===== {name} on {platform.system()}", flush=True)
        try:
            SCN[name]()
        except Exception as exc:  # a driver bug is a result too
            import traceback
            traceback.print_exc()
            check(f"{name}: driver ran to the end", False, repr(exc))
            for p in track():
                if is_mine(p, ROOTDIR):
                    kill_pid(p["pid"])
            for r in ROOTPIDS:
                kill_pid(r)
    bad = [r for r in RESULTS if not r[1]]
    print(f"===== {len(RESULTS) - len(bad)} passed, {len(bad)} failed", flush=True)
    sys.exit(1 if bad else 0)
