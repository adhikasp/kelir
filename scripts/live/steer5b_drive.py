"""STEER-5 round-2 live driver: verbs/signals vs a retry wait, hints, leak, tool-started cut.

Usage: python steer5b_drive.py <scenario> <kelir-binary> <scratch-dir>
Scenarios: stop now model later end sigint hint0 hintpast hintneg hintgarbage leak leak400 tool
Canned server only (no keys). Kills only the pids it started. Prints one RESULT line per scenario.
"""
import glob
import json
import os
import random
import shutil
import signal
import subprocess
import sys
import time

SC, BIN, SCRATCH = sys.argv[1], sys.argv[2], sys.argv[3]
D = os.path.dirname(os.path.abspath(__file__))
W = os.path.join(SCRATCH, SC)
shutil.rmtree(W, ignore_errors=True)
os.makedirs(W + "/work")
os.makedirs(W + "/cfg")
FAKEKEY = "sk-or-v1-REVIEWFAKEKEY0123456789abcdef"
PORT = 21000 + random.randrange(1500)
LOG = W + "/requests.jsonl"
NOWIN = getattr(subprocess, "CREATE_NO_WINDOW", 0)
HINT_SECS = float(os.environ.get("HINT_SECS", "8"))
kids = []


def spawn(args, **kw):
    p = subprocess.Popen(args, creationflags=NOWIN, **kw)
    kids.append(p)
    return p


def reqs():
    try:
        return [json.loads(line) for line in open(LOG, encoding="utf-8")]
    except OSError:
        return []


def events():
    f = glob.glob(W + "/state/sessions/*/events.jsonl")
    if not f:
        return []
    out = []
    for line in open(f[0], encoding="utf-8", errors="replace"):
        try:
            out.append(json.loads(line))
        except ValueError:
            pass
    return out


def sid_now():
    return os.path.basename(os.path.dirname(glob.glob(W + "/state/sessions/*/events.jsonl")[0]))


def until(pred, timeout):
    end = time.time() + timeout
    while time.time() < end:
        v = pred()
        if v:
            return v
        time.sleep(0.02)
    return None


def has(t):
    return next((e for e in events() if e.get("type") == t), None)


def ctl(*a):
    t = time.time()
    r = subprocess.run([BIN, "ctl", sid_now(), *a], capture_output=True, text=True, env=ENV,
                       creationflags=NOWIN, timeout=30)
    return time.time() - t, (r.stdout + r.stderr).strip()


SCRIPTS = {
    "stop": "429:60;ok", "now": "429:60;ok", "model": "429:60;ok", "later": "429:60;ok",
    "end": "429:60;ok", "sigint": "429:60;ok",
    "hint0": ";".join(["429:0"] * 400), "hintpast": ";".join(["429:Mon, 01 Jan 2001 00:00:00 GMT"] * 400),
    "hintneg": ";".join(["429:-5"] * 400), "hintgarbage": ";".join(["429:soon"] * 400),
    "leak": "429:echo;ok", "leak400": "400:echo", "tool": "tool;ok",
}
spawn([sys.executable, D + "/steer5b_server.py", str(PORT), LOG, SCRIPTS[SC]],
      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(0.8)
ENV = dict(os.environ, KELIR_STATE_DIR=W + "/state", KELIR_BASE_URL=f"http://127.0.0.1:{PORT}/api/v1",
           OPENROUTER_API_KEY=FAKEKEY)
open(W + "/cfg/config.toml", "w").write("[retry]\nmax_wait = 600\n")
out = open(W + "/out.ndjson", "wb")
err = open(W + "/err.txt", "wb")
k = spawn([BIN, "run", "--keep", "--json", "--config", W + "/cfg/config.toml", "--model", "test/m1", "go"],
          cwd=W + "/work", env=ENV, stdout=out, stderr=err)
res = {"scenario": SC}
try:
    if SC in ("stop", "now", "model", "later", "end", "sigint"):
        until(lambda: any("retry_in_ms" in e for e in events()), 15)
        res["retry_in_ms"] = next(e["retry_in_ms"] for e in events() if "retry_in_ms" in e)
        n0 = len(reqs())
        t0 = time.time()
        if SC == "stop":
            dt, ack = ctl("stop", "enough")
            ev = until(lambda: has("turn_ended"), 5)
            res.update(ack=ack, ack_s=round(dt, 2), outcome=ev and ev["outcome"],
                       effect_s=ev and round(time.time() - t0, 2))
        elif SC == "now":
            dt, ack = ctl("--now", "say", "change of plan: say hi")
            r2 = until(lambda: len(reqs()) > n0, 5)
            res.update(ack=ack, ack_s=round(dt, 2), second_request_s=r2 and round(time.time() - t0, 2))
            ev = until(lambda: has("turn_ended"), 10)
            res.update(outcome=ev and ev["outcome"], users=[e.get("text") for e in events() if e.get("type") == "user"])
        elif SC == "model":
            dt, ack = ctl("model", "test/m2")
            r2 = until(lambda: len(reqs()) > n0, 5)
            res.update(ack=ack, ack_s=round(dt, 2), second_request_s=r2 and round(time.time() - t0, 2),
                       second_model=r2 and reqs()[n0]["model"])
            ev = until(lambda: has("turn_ended"), 10)
            res.update(outcome=ev and ev["outcome"])
        elif SC == "later":
            dt, ack = ctl("later", "after this one: say bye")
            time.sleep(3)
            res.update(ack=ack, ack_s=round(dt, 2), requests_3s_later=len(reqs()) - n0,
                       turn_ended_3s=bool(has("turn_ended")))
            dt2, ack2 = ctl("stop", "ok")
            ev = until(lambda: has("turn_ended"), 5)
            res.update(stop_ack=ack2, outcome=ev and ev["outcome"])
            time.sleep(1.5)
            res["after_stop_requests"] = len(reqs()) - n0
            res["users"] = [e.get("text") for e in events() if e.get("type") == "user"]
            res["turn_endeds"] = [e.get("outcome") for e in events() if e.get("type") == "turn_ended"]
        elif SC == "end":
            dt, ack = ctl("end")
            ok = until(lambda: k.poll() is not None, 5)
            res.update(ack=ack, ack_s=round(dt, 2), exit_code=k.poll(), exit_s=ok and round(time.time() - t0, 2),
                       turn_endeds=[e.get("outcome") for e in events() if e.get("type") == "turn_ended"],
                       requests=len(reqs()) - n0)
        elif SC == "sigint":
            time.sleep(float(os.environ.get("SIG_DELAY", "0")))
            t0 = time.time()
            os.kill(k.pid, getattr(signal, os.environ.get("SIG", "SIGINT")))
            ev = until(lambda: has("turn_ended"), 5)
            res.update(outcome=ev and ev["outcome"], effect_s=ev and round(time.time() - t0, 2),
                       alive_after=k.poll() is None, requests=len(reqs()) - n0)
            if k.poll() is None:
                ctl("end")
                until(lambda: k.poll() is not None, 5)
                res["exit_after_end"] = k.poll()
    elif SC.startswith("hint"):
        time.sleep(HINT_SECS)
        r = reqs()
        ts = [x["t"] for x in r]
        res.update(requests=len(r), over_s=HINT_SECS,
                   min_gap_ms=int(1000 * min(b - a for a, b in zip(ts, ts[1:]))) if len(ts) > 1 else None,
                   retry_in_ms=[e.get("retry_in_ms") for e in events() if e.get("type") == "error"][:8])
    elif SC in ("leak", "leak400", "tool"):
        until(lambda: has("turn_ended"), 25)
        res["requests"] = len(reqs())
        res["turn_ended"] = [(e.get("outcome"), e.get("reason")) for e in events() if e.get("type") == "turn_ended"]
        res["errors"] = [{x: e.get(x) for x in ("where", "retryable", "status", "message")}
                         for e in events() if e.get("type") == "error"]
        res["tool_ran_file"] = os.path.exists(W + "/work/tool_ran.txt")
        res["tool_events"] = [e.get("type") for e in events() if "tool" in e.get("type", "")]
        if k.poll() is None:
            ctl("end")
            until(lambda: k.poll() is not None, 5)
        leak = []
        for root, _, fs in os.walk(W):
            for f in fs:
                if f == "requests.jsonl":
                    continue
                p = os.path.join(root, f)
                try:
                    if FAKEKEY.encode() in open(p, "rb").read():
                        leak.append(os.path.relpath(p, W))
                except OSError:
                    pass
        res["files_containing_key"] = leak
        res["redacted_marker_seen"] = any("[redacted]" in (e.get("message") or "") for e in events())
finally:
    for p in kids:
        if p.poll() is None:
            try:
                p.terminate()
                p.wait(5)
            except Exception:
                p.kill()
print("RESULT", json.dumps(res, default=str))
