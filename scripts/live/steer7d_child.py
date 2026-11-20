#!/usr/bin/env python3
"""steer7d review: what a token-holding tool child can learn or forge, run from INSIDE a session (a bash tool call).

Usage (as the model's bash command): python -I steer7d_child.py <memscan.py>
Reads KELIR_SOCK / KELIR_TOKEN / KELIR_SESSION_DIR like any child. Prints one JSON line per attack; secrets are
never printed (only booleans and lengths). Windows pipe transport and Unix sockets both supported.
"""
import json, os, re, subprocess, sys, time, glob, socket

WIN = os.name == "nt"
SOCK, TOKEN = os.environ["KELIR_SOCK"], os.environ["KELIR_TOKEN"]
SDIR = os.environ.get("KELIR_SESSION_DIR", "")
MEMSCAN = sys.argv[1] if len(sys.argv) > 1 else None
res = []


def say(name, **kw):
    kw["attack"] = name
    print(json.dumps(kw), flush=True)
    res.append(kw)


def talk(req, timeout=8.0):
    line = (json.dumps(req) + "\n").encode()
    if WIN:
        deadline = time.time() + timeout
        while True:
            try:
                f = open(SOCK, "r+b", buffering=0)
                break
            except OSError:
                if time.time() > deadline:
                    return {"transport": "unreachable"}
                time.sleep(0.05)
        f.write(line)
        out = b""
        while not out.endswith(b"\n"):
            chunk = f.read(4096)
            if not chunk:
                break
            out += chunk
        f.close()
    else:
        s = socket.socket(socket.AF_UNIX)
        s.settimeout(timeout)
        s.connect(SOCK)
        s.sendall(line)
        out = b""
        while not out.endswith(b"\n"):
            chunk = s.recv(4096)
            if not chunk:
                break
            out += chunk
        s.close()
    try:
        return json.loads(out.decode())
    except Exception:
        return {"unparsed": True}


def secretish(answer):
    text = json.dumps(answer)
    return bool(re.search(r"[0-9a-f]{64}", text)) or '"capability"' in text or '"key"' in text


def read_log():
    return open(os.path.join(SDIR, "events.jsonl"), encoding="utf-8", errors="replace").read()


bg = os.path.join(SDIR, "bg")
ids = sorted({os.path.basename(p).split(".")[0] for p in glob.glob(os.path.join(bg, "bg_*.status"))})
say("jobs-visible", count=len(ids))
job = ids[0] if ids else None
if job:
    out = os.path.join(bg, job + ".out")
    a = talk({"verb": "bg-spawn", "from": "job", "token": TOKEN,
              "args": {"dir": bg, "id": job, "out": out, "cmd": "echo x", "shell": [], "kind": "job"}})
    say("respawn-named-id", ok=a.get("ok"), leaked=secretish(a), error=(a.get("error") or "")[:90])
c = talk({"verb": "bg-claim", "from": "job", "token": TOKEN, "args": {"call": None}})
say("bg-claim", ok=c.get("ok"), leaked=secretish(c), state_keys=sorted((c.get("state") or {}).keys()))
if job:
    st = json.load(open(os.path.join(bg, job + ".status")))
    st["state"], st["exit_code"], st["finished"] = "exited", 0, "2026-11-20T00:00:00Z"
    st.pop("sig", None)
    f = talk({"verb": "bg", "from": "job", "token": TOKEN, "capability": "00" * 24, "args": st})
    say("forged-report-guessed-capability", ok=f.get("ok"), error=(f.get("error") or "")[:90])
    # a refused spawn must not put the environment or the token in the log
    canary = "D7CANARY" + str(os.getpid())
    r = talk({"verb": "bg-spawn", "from": "job", "token": TOKEN,
              "args": {"dir": bg, "out": "", "cmd": "echo x", "shell": [], "kind": "job", "token": TOKEN,
                       "cwd": "no-such-dir-7d",
                       "env": [["SOME_API_KEY", canary]] + list(os.environ.items())}})
    time.sleep(0.5)
    log = read_log()
    # the environment `kelir bg run` itself sends: does a provider key of the session reach the log?
    keyvars = [k for k, v in os.environ.items() if "API_KEY" in k.upper() and len(v) > 8 and v in log]
    say("refused-spawn-env-in-log", ok=r.get("ok"), canary_in_log=canary in log, token_in_log=TOKEN in log,
        provider_key_vars_in_log=keyvars)
    # an unsigned final sidecar with a huge sequence number
    st2 = dict(st, seq=2 ** 63, cmd="FORGED-7d")
    pth = os.path.join(bg, job + ".final")
    json.dump(st2, open(pth, "w"))
    time.sleep(1.5)
    say("planted-unsigned-final", forged_in_log="FORGED-7d" in read_log())
    try:
        os.remove(pth)
    except OSError:
        pass

# memory scan of the session process (the store of every job's key)
if WIN and MEMSCAN:
    import ctypes
    from ctypes import wintypes as w

    class PE(ctypes.Structure):
        _fields_ = [("dwSize", w.DWORD), ("cntUsage", w.DWORD), ("th32ProcessID", w.DWORD),
                    ("th32DefaultHeapID", ctypes.c_size_t), ("th32ModuleID", w.DWORD), ("cntThreads", w.DWORD),
                    ("th32ParentProcessID", w.DWORD), ("pcPriClassBase", ctypes.c_long), ("dwFlags", w.DWORD),
                    ("szExeFile", ctypes.c_wchar * 260)]

    k = ctypes.WinDLL("kernel32", use_last_error=True)
    k.CreateToolhelp32Snapshot.restype = w.HANDLE
    snap = k.CreateToolhelp32Snapshot(2, 0)
    pe = PE()
    pe.dwSize = ctypes.sizeof(pe)
    procs = {}
    ok = k.Process32FirstW(snap, ctypes.byref(pe))
    while ok:
        procs[pe.th32ProcessID] = (pe.th32ParentProcessID, pe.szExeFile.lower())
        ok = k.Process32NextW(snap, ctypes.byref(pe))
    pid, hops, sess = os.getpid(), 0, None
    while pid in procs and hops < 12:
        ppid, exe = procs[pid]
        if exe == "kelir.exe":
            sess = pid
            break
        pid, hops = ppid, hops + 1
    if sess:
        keyfile = os.path.join(os.environ.get("TEMP", "."), "d7-session.key")
        p = subprocess.run([sys.executable, "-I", MEMSCAN, str(sess), keyfile], capture_output=True, text=True)
        found = os.path.exists(keyfile)
        if found:
            os.remove(keyfile)
        say("memscan-session", found=found, said=p.stdout.strip()[:80])
    else:
        say("memscan-session", found=None, said="session pid not found")
bad = [r["attack"] for r in res if r.get("leaked") or r.get("found") or r.get("forged_in_log")
       or r.get("canary_in_log") or r.get("token_in_log") or r.get("provider_key_vars_in_log")]
print("SUMMARY " + json.dumps(bad))
