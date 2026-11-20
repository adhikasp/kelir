#!/usr/bin/env python3
"""STEER-7 review: adversarial attacker against a running kelir session's socket.

Run as `python -I steer7_attack.py <mode> <state-dir> <target-session-id> [victim-session-id]`.
Modes (each is a different attacker vantage point; no key or secret is ever printed):

  stranger   knows the pipe/socket name only (a console-less process on Windows, a second user or a
             second shell on Linux). No token. Tries every verb with no / empty / wrong / mistyped tokens.
  reader     same user, no env, reads `<state>/sessions/<id>/token` (the file `kelir ctl` reads). Reports
             whether the file is readable and what it can then do.  (Documented as trusted by design.)
  child      runs INSIDE the session (a bash tool call): holds KELIR_TOKEN/KELIR_SOCK like any test child.
             Tries to impersonate the bg shim, escape bg/, cancel the victim session's job.

Every attack prints one JSON line {"attack","ok","error","new_events"}; a `verb` event with
boundary `rejected` is a *refusal* and not an effect (an authenticated attempt is logged by
design), so `new_events` names it as such and only an accepted verb or a job event counts.
The summary line `SUMMARY exploits=[...]` lists the attacks that were accepted or changed state.

`reader` is expected to have one: a same-user process can read the token file, which is the
documented same-user trust boundary (SPEC/DEVIATIONS), and cannot read a job's capability.
"""
import json, os, sys, time, glob, socket, subprocess

IS_WIN = os.name == "nt"
mode, state, tid = sys.argv[1], sys.argv[2], sys.argv[3]
vid = sys.argv[4] if len(sys.argv) > 4 else None
SESS = os.path.join(state, "sessions")
exploits = []


def addr(sid):
    return r"\\.\pipe\kelir-" + sid if IS_WIN else os.path.join(SESS, sid, "sock")


def talk(address, line: bytes, timeout=8.0, close_after=True):
    """One raw line in, one line out."""
    if IS_WIN:
        import msvcrt  # noqa
        deadline = time.time() + timeout
        while True:
            try:
                f = open(address, "r+b", buffering=0)
                break
            except OSError as e:
                if time.time() > deadline:
                    return {"transport_error": str(e)}
                time.sleep(0.05)
        try:
            # The write itself is bounded: a peer that refuses the line at its
            # cap stops reading, and an unbounded write would then block the
            # attacker, not the session (STEER-7 fixes: this hung the drive).
            sent = {"done": False}

            def writer():
                try:
                    f.write(line)
                    sent["done"] = True
                except OSError as e:
                    sent["error"] = str(e)

            import threading
            th = threading.Thread(target=writer, daemon=True)
            th.start()
            th.join(min(timeout, 10.0))
            out = b""
            try:
                while not out.endswith(b"\n"):
                    chunk = f.read(4096)
                    if not chunk:
                        break
                    out += chunk
            except OSError as e:
                out = b'{"transport_error": "' + str(e).encode() + b'"}'
            if not sent["done"] and not out:
                out = b'{"transport_error": "write blocked after the peer stopped reading"}'
            return out
        finally:
            f.close()
    s = socket.socket(socket.AF_UNIX)
    s.settimeout(timeout)
    try:
        s.connect(address)
        s.sendall(line)
        out = b""
        while not out.endswith(b"\n"):
            chunk = s.recv(4096)
            if not chunk:
                break
            out += chunk
        return out
    except OSError as e:
        return {"transport_error": str(e)}
    finally:
        s.close()


def log_lines(sid):
    p = os.path.join(SESS, sid, "events.jsonl")
    try:
        with open(p, "rb") as f:
            return f.read().splitlines()
    except OSError:
        return []


def send(sid, req, label, token_probe=None):
    """Send `req` (dict) to session sid; report answer and effect on its log."""
    before = log_lines(sid)
    raw = talk(addr(sid), (json.dumps(req) + "\n").encode())
    time.sleep(0.4)
    after = log_lines(sid)
    new = after[len(before):]
    types = []
    # A refusal is *logged* by design (an authenticated caller's attempt is a
    # `verb` event with boundary `rejected`), so it is not an effect: only an
    # accepted verb, or a job event, is. Counting the refusal line itself as an
    # exploit made every correctly refused attack look like one (STEER-7 fixes).
    effects = []
    for l in new:
        try:
            e = json.loads(l)
            rejected = e.get("type") == "verb" and e.get("boundary") == "rejected"
            name = e.get("type") + (":" + e.get("name", "") if e.get("type") == "verb" else "")
            name += ":job" if e.get("type") == "user" and e.get("from") == "job" else ""
            types.append(name + ("(rejected)" if rejected else ""))
            if not rejected:
                effects.append(name)
        except Exception:
            types.append("?")
            effects.append("?")
    if isinstance(raw, dict):
        ans = raw
    else:
        try:
            ans = json.loads(raw.decode("utf-8", "replace").strip() or "null")
        except Exception:
            ans = {"unparsed": raw[:120].decode("utf-8", "replace")}
    ok = isinstance(ans, dict) and ans.get("ok") is True
    changed = bool(effects)
    rec = {"attack": label, "ok": ok, "error": (ans or {}).get("error", "")[:140] if isinstance(ans, dict) else "",
           "new_events": types}
    # Never print a secret: scrub the token if an answer ever echoed it.
    s = json.dumps(rec)
    for t in secrets:
        if t and t in s:
            s = s.replace(t, "<TOKEN-LEAKED-IN-ANSWER>")
            exploits.append(label + " (token echoed)")
    print(s, flush=True)
    if ok or changed:
        exploits.append(label + ("" if ok else " (state changed)"))
    return ans


secrets = []


def token_file(sid):
    try:
        return open(os.path.join(SESS, sid, "token")).read().strip()
    except OSError:
        return None


def stranger():
    v = "bg_" + "0" * 26
    reqs = [
        ("bg-forged-lost-notoken", {"verb": "bg", "from": "job", "args": {"id": v, "state": "lost", "started": "2026-01-01T00:00:00Z", "out": "x", "kind": "job"}}),
        ("say-notoken", {"verb": "say", "args": {"text": "pwned"}, "from": "ctl"}),
        ("stop-notoken", {"verb": "stop", "args": {}, "from": "ctl"}),
        ("end-notoken", {"verb": "end", "args": {}, "from": "ctl"}),
        ("model-notoken", {"verb": "model", "args": {"model": "x/y"}, "from": "ctl"}),
        ("compact-notoken", {"verb": "compact", "args": {}, "from": "ctl"}),
        ("cancel-notoken", {"verb": "cancel", "args": {"id": "x"}, "from": "ctl"}),
        ("bg-spawn-notoken", {"verb": "bg-spawn", "from": "job", "args": {"dir": "C:\\Windows\\Temp", "id": "bg_evil", "out": "o", "cmd": "calc", "shell": ["cmd", "/c"], "kind": "job"}}),
    ]
    for label, r in reqs:
        send(tid, r, label)
    base = {"verb": "say", "args": {"text": "pwned"}, "from": "ctl"}
    for label, tok in [("token-empty", ""), ("token-space", " "), ("token-null", None), ("token-int0", 0),
                       ("token-list", ["a"]), ("token-wrong64", "0" * 64), ("token-true", True),
                       ("token-other-session-ish", "f" * 64)]:
        r = dict(base); r["token"] = tok
        send(tid, r, "say-" + label)
    # Smuggling: token inside args, and as a header-style field name variations
    for label, extra in [("args.token", {"args": {"text": "pwned", "token": "x"}}),
                         ("Token-capital", {"Token": "x"}), ("auth", {"auth": "x"}),
                         ("from=tui", {"from": "tui"}), ("from=session", {"from": "session"})]:
        r = dict(base); r.update(extra)
        send(tid, r, "say-" + label)
    # Malformed / oversized / split lines must not get past the gate either.
    send(tid, {"verb": "say", "args": {"text": "pwned"}, "from": "ctl", "token": None}, "say-token-null-explicit")
    # Two requests in one write: the first unauthenticated closes the connection - the 2nd must not run.
    before = log_lines(tid)
    raw = talk(addr(tid), (json.dumps(base) + "\n" + json.dumps(dict(base, token=token_file(tid) or "")) + "\n").encode())
    # (the second line carries the real token only if the file is readable: read via 'reader' semantic; here it
    # proves pipelining after a refusal does not get processed)
    time.sleep(0.4)
    after = log_lines(tid)
    print(json.dumps({"attack": "pipelined-refused-then-valid", "answer_first": (raw[:100].decode("utf-8", "replace") if isinstance(raw, bytes) else raw), "new_events": len(after) - len(before)}), flush=True)
    # Resource: 1 MB junk without a newline, from a child process with a deadline.
    # In this process a blocked write would also hold the file object's lock, so
    # the read that should collect the refusal could never run; a peer that does
    # not read cannot be told anything anyway, so what is checked is that the
    # session stays responsive and that nothing entered its log.
    before = log_lines(tid)
    t0 = time.time()
    # Its own connection and its own thread: a peer that does not read cannot be
    # told anything, so the write may block for ever. That is the attacker's
    # problem, not the session's, and the thread is left to die with us.
    def flood():
        try:
            f = open(addr(tid), "r+b", buffering=0)
            try:
                f.write(b"A" * (1 << 20) + b"\n")
            finally:
                try:
                    f.close()
                except OSError:
                    pass
        except OSError:
            pass

    import threading
    th = threading.Thread(target=flood, daemon=True)
    th.start()
    th.join(15)
    print(json.dumps({"attack": "1MB-junk-line",
                      "result": "write done" if not th.is_alive()
                      else "write still blocked (the session stopped reading at its cap)",
                      "secs": round(time.time() - t0, 2)}), flush=True)
    # The session must still serve: an unauthenticated request is answered at
    # once (that is the listener, not the log), and the junk changed nothing.
    time.sleep(0.5)
    after = log_lines(tid)
    print(json.dumps({"attack": "1MB-junk-line-effects", "new_events": len(after) - len(before)}), flush=True)
    t1 = time.time()
    probe = talk(addr(tid), (json.dumps({"verb": "say", "from": "ctl",
                                         "args": {"text": "still here"}}) + "\n").encode(), timeout=10)
    answered = isinstance(probe, bytes) and b"\"ok\":false" in probe
    print(json.dumps({"attack": "1MB-junk-line-then-a-request", "answered": answered,
                      "secs": round(time.time() - t1, 2)}), flush=True)


def reader():
    tok = token_file(tid)
    print(json.dumps({"attack": "read-token-file", "readable": tok is not None, "len": len(tok) if tok else 0}), flush=True)
    if tok:
        secrets.append(tok)
        send(tid, {"verb": "say", "args": {"text": "from-token-file-reader"}, "from": "ctl", "token": tok}, "say-with-file-token")


def child():
    tok = os.environ.get("KELIR_TOKEN")
    sock = os.environ.get("KELIR_SOCK")
    print(json.dumps({"attack": "env", "KELIR_TOKEN_set": bool(tok), "KELIR_SOCK_set": bool(sock)}), flush=True)
    if tok:
        secrets.append(tok)
    sdir = os.path.join(SESS, tid)
    # (a1) impersonate the shim for a job id that merely has a commands/<id>/ directory (a normal tool call).
    cmds = [d for d in glob.glob(os.path.join(sdir, "commands", "*")) if os.path.isdir(d)]
    if cmds:
        jid = os.path.basename(cmds[0])
        rep = {"verb": "bg", "from": "job", "token": tok,
               "args": {"id": jid, "state": "lost", "started": "2026-01-01T00:00:00Z",
                        "out": os.path.join(cmds[0], "out"), "kind": "job", "cmd": "forged"}}
        send(tid, rep, "a1-forge-bg-lost-for-existing-commands-dir:" + jid)
    # (a2) fabricate the ownership evidence: mkdir commands/<fake>, then report
    fake = "bg_fake_by_child"
    os.makedirs(os.path.join(sdir, "commands", fake), exist_ok=True)
    rep = {"verb": "bg", "from": "job", "token": tok,
           "args": {"id": fake, "state": "lost", "started": "2026-01-01T00:00:00Z",
                    "out": os.path.join(sdir, "commands", fake, "out"), "kind": "job", "cmd": "forged"}}
    send(tid, rep, "a2-forge-bg-lost-after-mkdir-commands-fake")
    # (a3) a totally unknown id, no evidence at all: must be refused
    rep["args"]["id"] = "bg_never_existed"
    send(tid, rep, "a3-forge-bg-unknown-id(control, expect refused)")
    # (a4) forge by dropping a status file in bg/
    fake2 = "bg_fake_status"
    with open(os.path.join(sdir, "bg", fake2 + ".status"), "w") as f:
        json.dump({"id": fake2, "state": "exited", "exit_code": 7, "started": "2026-01-01T00:00:00Z",
                   "finished": "2026-01-01T00:00:01Z", "cmd": "forged", "out": os.path.join(sdir, "bg", fake2 + ".out"), "kind": "job"}, f)
    send(tid, {"verb": "bg", "from": "job", "token": tok,
               "args": {"id": fake2, "state": "exited", "started": "2026-01-01T00:00:00Z", "out": os.path.join(sdir, "bg", fake2 + ".out"), "kind": "job"}},
         "a4-forge-via-status-file-drop")
    # (e) path escapes via bg-spawn and bg report (control: all must be refused; nothing may be spawned)
    junction = os.path.join(sdir, "bg", "evil_junction")
    target_out = os.path.join(os.environ.get("TEMP", "/tmp"), "steer7_escape_target")
    os.makedirs(target_out, exist_ok=True)
    try:
        if IS_WIN:
            os.system('cmd /c mklink /J "%s" "%s" >nul' % (junction, target_out))
        else:
            os.symlink(target_out, junction)
    except Exception as e:
        print("junction failed", e)
    marker = "steer7_spawn_marker"
    spawn_base = {"id": "bg_esc", "out": os.path.join(sdir, "bg", "bg_esc.out"),
                  "cmd": "echo spawned > %s" % os.path.join(target_out, marker).replace("\\", "/"),
                  "shell": ["bash", "-c"], "kind": "job"}
    dirs = {"dotdot": os.path.join(sdir, "bg", ".."), "relative-dotdot": "../..", "parent": sdir,
            "other-session": os.path.join(SESS, vid, "bg") if vid else os.path.join(SESS, "x"),
            "junction": junction, "junction-sub": os.path.join(junction, "sub"),
            "temp": target_out, "root": os.path.abspath(os.sep),
            "unc-localhost": r"\\localhost\c$\Windows\Temp", "unc-q": r"\\?\C:\Windows\Temp",
            "unc-slash": "//localhost/c$/Windows/Temp", "other-drive": "D:\\x" if IS_WIN else "/dev",
            "dotdot-backslash": "..\\..\\x", "bg-prefix-sibling": os.path.join(sdir, "bg_evil"),
            "bg-case": os.path.join(sdir, "BG") if IS_WIN else os.path.join(sdir, "bg", "..", "bg2")}
    for label, d in dirs.items():
        args = dict(spawn_base, dir=d, token=tok)
        send(tid, {"verb": "bg-spawn", "from": "job", "token": tok, "args": args}, "e-bg-spawn-dir:" + label)
    # out outside while dir inside bg
    for label, o in {"out-outside": os.path.join(target_out, "o"), "out-dotdot": os.path.join(sdir, "bg", "..", "x"),
                     "out-junction": os.path.join(junction, "o"), "out-unc": r"\\localhost\c$\x"}.items():
        args = dict(spawn_base, dir=os.path.join(sdir, "bg"), out=o, token=tok)
        send(tid, {"verb": "bg-spawn", "from": "job", "token": tok, "args": args}, "e-bg-spawn-" + label)
    # id traversal
    for label, i in {"id-dotdot": "../x", "id-slash": "a/b", "id-backslash": "a\\b", "id-colon-ads": "a:b",
                     "id-nul-device": "nul", "id-con": "con", "id-trailing-dot": "bg_esc.", "id-long": "a" * 300}.items():
        args = dict(spawn_base, dir=os.path.join(sdir, "bg"), id=i, token=tok)
        send(tid, {"verb": "bg-spawn", "from": "job", "token": tok, "args": args}, "e-bg-spawn-" + label)
    print(json.dumps({"attack": "spawn-marker-written", "exists": os.path.exists(os.path.join(target_out, marker))}), flush=True)
    # (b) cancel / report against another session using only what a child can see
    if vid:
        vsess = os.path.join(SESS, vid)
        vjobs = [os.path.basename(p)[:-7] for p in glob.glob(os.path.join(vsess, "bg", "*.status"))]
        print(json.dumps({"attack": "victim-jobs-visible", "n": len(vjobs)}), flush=True)
        for j in vjobs[:1]:
            # own token against the victim's socket: wrong token
            send(vid, {"verb": "cancel", "from": "ctl", "token": tok, "args": {"id": j}}, "b1-cancel-victim-job-with-own-token")
            # own token + own socket, victim id
            send(tid, {"verb": "cancel", "from": "ctl", "token": tok, "args": {"id": j}}, "b2-cancel-victim-job-via-own-session")
            send(tid, {"verb": "bg", "from": "job", "token": tok, "args": {"id": j, "state": "lost", "started": "x", "out": "o", "kind": "job"}}, "b3-report-victim-job-to-own-session")
        vtok = token_file(vid)
        print(json.dumps({"attack": "b4-victim-token-file-readable-by-child", "readable": vtok is not None}), flush=True)
        if vtok:
            secrets.append(vtok)
            for j in vjobs[:1]:
                send(vid, {"verb": "cancel", "from": "ctl", "token": vtok, "args": {"id": j}}, "b4-cancel-victim-job-with-victim-token-file")
            send(vid, {"verb": "say", "from": "ctl", "token": vtok, "args": {"text": "steered by sibling child"}}, "b4-say-victim-with-victim-token-file")


def foreign():
    """Authenticated to the target (token read from its file) but naming the VICTIM's real bg_ job."""
    tok = token_file(tid)
    secrets.append(tok or "")
    vjobs = [os.path.basename(p)[:-7] for p in glob.glob(os.path.join(SESS, vid, "bg", "bg_*.status"))]
    print(json.dumps({"attack": "victim-real-bg-ids", "n": len(vjobs)}), flush=True)
    for j in vjobs[:1]:
        send(tid, {"verb": "cancel", "from": "ctl", "token": tok, "args": {"id": j}}, "f1-cancel-victim-real-job-via-target")
        send(tid, {"verb": "bg", "from": "job", "token": tok, "args": {"id": j, "state": "lost", "started": "x", "out": os.path.join(SESS, vid, "bg", j + ".out"), "kind": "job"}}, "f2-report-victim-real-job-to-target")
        send(tid, {"verb": "bg-spawn", "from": "job", "token": tok, "args": {"dir": os.path.join(SESS, vid, "bg"), "id": j, "out": os.path.join(SESS, vid, "bg", j + ".out"), "cmd": "echo hi", "shell": ["bash", "-c"], "kind": "job"}}, "f3-spawn-into-victim-bg-dir")


{"stranger": stranger, "reader": reader, "child": child, "foreign": foreign}[mode]()
print("SUMMARY exploits=" + json.dumps(exploits), flush=True)
