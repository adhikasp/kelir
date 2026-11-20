"""M5-B live drive (reviewer): Herdr line, hint (10 held-out prompts), cache prefix, observe vs wire.

Usage (inside a Herdr pane, OPENROUTER_API_KEY in the environment):
  python m5b_live.py <scratch-dir> [kelir-binary]
Runs the real binary against OpenRouter through scripts/live/m5b_proxy.py. Never prints the key.
"""
import json, os, subprocess, sys, time, shutil, socket, pathlib

S = pathlib.Path(sys.argv[1]).resolve() / "m5b"
BIN = sys.argv[2] if len(sys.argv) > 2 else str(pathlib.Path(__file__).resolve().parents[2] / "target/release/kelir.exe")
HERE = pathlib.Path(__file__).resolve().parent
MODEL = os.environ.get("KELIR_LIVE_MODEL", "deepseek/deepseek-v4.1-flash-1017")
KEY = os.environ["OPENROUTER_API_KEY"]
shutil.rmtree(S, ignore_errors=True)
for d in ("proj/.claude/skills/sift", "proj/.claude/skills/changelog", "proj/.claude/skills/csv-clean", "home", "cfg", "mem", "state"):
    (S / d).mkdir(parents=True)


def w(p, t):
    (S / p).write_text(t, encoding="utf-8", newline="\n")


w("proj/.claude/skills/sift/SKILL.md", "---\nname: sift\ndescription: use when a log is too long to read\n---\n\n# Sift\n\nRead the tail first. SIFT-BODY-7781\n")
w("proj/.claude/skills/changelog/SKILL.md", "---\nname: changelog\ndescription: draft release notes or a changelog entry from git history\n---\n\n# Changelog\n\nCHANGELOG-BODY-5512\n")
w("proj/.claude/skills/csv-clean/SKILL.md", "---\nname: csv-clean\ndescription: clean and normalise messy CSV files, dedupe rows, fix column formats\n---\n\n# CSV\n\nCSV-BODY-3390\n")
pad = "".join(f"\n- Rule {i}: keep steps small, verify what you changed, and put long output in a file." for i in range(160))
w("proj/AGENTS.md", "# Project rules\n" + pad + "\n")
w("mem/notes.md", "# Notes\n\nThe memory codeword is GUAVA-2210.\n")
w("cfg/config.toml", f'memory_dir = "{(S / "mem").as_posix()}"\n[hint]\nmodel = "{MODEL}"\n')
LOG = S / "requests.jsonl"
with socket.socket() as s:
    s.bind(("127.0.0.1", 0))
    PORT = s.getsockname()[1]
proxy = subprocess.Popen([sys.executable, str(HERE / "m5b_proxy.py"), str(PORT), str(LOG)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
env = dict(os.environ, KELIR_STATE_DIR=str(S / "state"), KELIR_HOME=str(S / "home"), KELIR_CONFIG_DIR=str(S / "cfg"),
           KELIR_BASE_URL=f"http://127.0.0.1:{PORT}/api/v1")
out = open(S / "report.txt", "w", encoding="utf-8")


def say(*a):
    line = " ".join(str(x) for x in a)
    print(line, flush=True)
    out.write(line + "\n")
    out.flush()


def kelir(*args, e=None, timeout=120):
    return subprocess.run([BIN, *args], env=e or env, capture_output=True, text=True, timeout=timeout, cwd=S / "proj", encoding="utf-8")


def events(sid_dir):
    p = sid_dir / "events.jsonl"
    return [json.loads(l) for l in p.read_text(encoding="utf-8").splitlines() if l.strip()] if p.exists() else []


def sess_dirs(root):
    sd = root / "sessions"
    return sorted(sd.iterdir(), key=lambda p: p.stat().st_mtime) if sd.exists() else []


def reqs():
    return [json.loads(l) for l in LOG.read_text(encoding="utf-8").splitlines() if l.strip()] if LOG.exists() else []


def sysline(ev):
    st = [e for e in ev if e["type"] == "session_started"][0]
    t = st["system"]["text"]
    t = t if isinstance(t, str) else t.get("preview", "")
    return st, [l for l in t.splitlines() if l.startswith("herdr:")]


def txt(t):
    return t if isinstance(t, str) else t.get("preview", "")


try:
    time.sleep(1)
    # ---- 1. Herdr line (in a pane) ------------------------------------------------
    tid = os.environ.get("HC_TERMINAL_ID", "")
    say("HC_TERMINAL_ID set:", bool(tid))
    subprocess.run(["hc", "label", tid, "herdr.space=spcREV", "herdr.tab=tbREV"], capture_output=True)
    hcget = json.loads(subprocess.run(["hc", "get", tid, "--json"], capture_output=True, text=True, encoding="utf-8").stdout)
    t0 = time.time()
    r = kelir("run", "--json", "--cwd", str(S / "proj"), "--model", MODEL, "Reply with the single word ok.")
    say("herdr run exit", r.returncode, "secs", round(time.time() - t0, 1))
    sd = sess_dirs(S / "state")[-1]
    ev = events(sd)
    st, _ = sysline(ev)
    ob0 = kelir("observe", sd.name).stdout
    lines = [l for l in ob0.splitlines() if l.startswith("herdr:")]
    say("herdr lines in system (from observe):", lines)
    say("hc get says: address", hcget["address"], "space", hcget["labels"].get("herdr.space"), "tab", hcget["labels"].get("herdr.tab"), "where", hcget["labels"].get("where"))
    say("sources[] herdr:", [(x["name"], x["path"]) for x in st["sources"] if x["name"] == "herdr"])
    ob = kelir("observe", sd.name).stdout
    say("observe shows herdr source:", "herdr" in ob.split("== hint")[0], "| observe has system line:", any(l in ob for l in lines))
    say("agent count words in line:", any(w_ in "\n".join(lines).lower() for w_ in ("agents", "peers", "live")))
    say("memory in system:", "GUAVA-2210" in ob0)
    e2 = {k: v for k, v in env.items() if k != "HC_TERMINAL_ID"}
    r = kelir("run", "--json", "--cwd", str(S / "proj"), "--model", MODEL, "Reply with the single word ok.", e=e2)
    sd2 = [d for d in sess_dirs(S / "state") if d != sd][-1]
    say("outside herdr lines:", [l for l in kelir("observe", sd2.name).stdout.splitlines() if l.startswith("herdr:")])
    say("hint calls so far (non-stream):", sum(1 for q in reqs() if not q["stream"]), "(2 runs, one prompt each => expect 2)")

    # ---- 2. hint session with 10 held-out prompts -----------------------------------
    PROMPTS = [
        ("build.log is 40000 lines and I cannot find why the CI failed", "sift"),
        ("write release notes for v2.3 from the commits since the last tag", "changelog"),
        ("this export.csv has duplicate rows and mixed date formats, tidy it", "csv-clean"),
        ("the test output scrolled past my terminal buffer, what failed first?", "sift"),
        ("summarise what changed between v1.0 and v1.1 for our users", "changelog"),
        ("normalise the phone number column in customers.csv", "csv-clean"),
        ("what is the capital of France", None),
        ("tail the huge server log and tell me the first stack trace", "sift"),
        ("prepare the CHANGELOG.md entry for this pull request", "changelog"),
        ("refactor this function to use iterators", None),
    ]
    SUF = " (Do not use tools. Answer in at most five words.)"
    n_before = len(reqs())
    p = subprocess.Popen([BIN, "run", "--keep", "--json", "--cwd", str(S / "proj"), "--model", MODEL, PROMPTS[0][0] + SUF],
                         env=env, stdout=open(S / "keep.ndjson", "w"), stderr=open(S / "keep.err", "w"), cwd=S / "proj")
    try:
        sd3 = None
        for _ in range(100):
            ds = [d for d in sess_dirs(S / "state") if d not in (sd, sd2)]
            if ds:
                sd3 = ds[-1]
                break
            time.sleep(0.2)

        def turns():
            return sum(1 for e in events(sd3) if e["type"] == "turn_ended")

        def wait_turns(n, t=120):
            end = time.time() + t
            while turns() < n and time.time() < end:
                time.sleep(0.3)
            return turns() >= n

        ok = wait_turns(1)
        for i, (pr, _) in enumerate(PROMPTS[1:], start=2):
            if not ok:
                break
            kelir("ctl", sd3.name, "say", pr + SUF)
            ok = wait_turns(i)
        say("hint session turns:", turns())
        rawat = kelir("observe", sd3.name, "--raw").stdout

        def held(label, action):
            """Hold the next streamed request at the proxy, run `action`, observe --raw, compare, release."""
            for ext in (".arrived",):
                try: os.remove(str(LOG) + ext)
                except OSError: pass
            open(str(LOG) + ".hold", "w").close()
            n0 = turns()
            action()
            end = time.time() + 60
            while not os.path.exists(str(LOG) + ".arrived") and time.time() < end:
                time.sleep(0.02)
            time.sleep(0.3)
            wire = open(str(LOG) + ".arrived", encoding="utf-8", newline="").read() if os.path.exists(str(LOG) + ".arrived") else None
            obs = kelir("observe", sd3.name, "--raw").stdout
            os.remove(str(LOG) + ".hold")
            wait_turns(n0 + 1)
            same = wire is not None and obs == wire
            say(f"observe --raw == wire body of the request held at the proxy [{label}]:", same)
            if wire is not None and not same:
                a, b = json.loads(obs), json.loads(wire)
                say("  differs: observe msgs", len(a["messages"]), "wire msgs", len(b["messages"]), "keys", sorted(set(a) ^ set(b)),
                    "same prefix:", a["messages"][:len(b["messages"])] == b["messages"][:len(a["messages"])])
            return same

        held("after a hint", lambda: kelir("ctl", sd3.name, "say", "refactor this function to use iterators" + SUF))
        def edit_src():
            (S / "proj/AGENTS.md").write_text("# Project rules (EDITED)\nThe codeword is MANGO.\n", encoding="utf-8")
            (S / "mem/notes.md").write_text("# Notes\nEDITED GUAVA-0001\n", encoding="utf-8")
            kelir("ctl", sd3.name, "say", "What is the codeword?" + SUF)
        held("after an AGENTS.md edit + memory edit", edit_src)
        held("after a model swap", lambda: (kelir("ctl", sd3.name, "model", MODEL), kelir("ctl", sd3.name, "say", "Say ok." + SUF)))

        def new_note():
            (S / "mem/new.md").write_text("fresh note PAPAYA-9\n", encoding="utf-8")
            kelir("ctl", sd3.name, "say", "Say ok." + SUF)
        held("after a new memory note", new_note)
        ev3 = events(sd3)
        hints = [e for e in ev3 if e["type"] == "source" and e.get("kind") == "hint"]
        texts = [txt(h["text"]) for h in hints]
        say("hint events:", len(hints), "for", len(PROMPTS) + 1, "prompts")
        user_seqs = [e["seq"] for e in ev3 if e["type"] == "user"]
        correct = 0
        scored = 0
        details = []
        for idx, (pr, exp) in enumerate(PROMPTS):
            if idx >= len(user_seqs):
                break
            lo = user_seqs[idx]
            hi = user_seqs[idx + 1] if idx + 1 < len(user_seqs) else 10 ** 9
            hs = [t for h, t in zip(hints, texts) if lo < h["seq"] < hi]
            named = [l.split("might be relevant: ")[1].split(" - ")[0] for t in hs for l in t.splitlines() if "might be relevant: " in l]
            good = (not named) if exp is None else (exp in named)
            scored += 1
            correct += good
            details.append((pr[:40], exp, named, good))
        for d in details:
            say("  ", d)
        say(f"hint quality: {correct}/{scored} right (a none-expected prompt is right when nothing is named)")
        say("hint text has any skill body:", any(b in "\n".join(texts) for b in ("SIFT-BODY", "CHANGELOG-BODY", "CSV-BODY")))
        main = [q for q in reqs()[n_before:] if q["stream"]]
        stable = 0
        for a, b in zip(main, main[1:]):
            ma, mb = json.loads(a["raw"])["messages"], json.loads(b["raw"])["messages"]
            if json.dumps(mb[:len(ma)]) == json.dumps(ma):
                stable += 1
        say("main requests:", len(main), "| pairs where request k+1 extends request k byte-for-byte:", stable, "of", max(len(main) - 1, 0))
        us = [e for e in ev3 if e["type"] == "usage" and e.get("kind") is None]
        say("per-step usage (prompt, cached):", [(u["prompt_tokens"], u["cached_tokens"]) for u in us])
        hintreq = [q for q in reqs()[n_before:] if not q["stream"]]
        say("hint calls:", len(hintreq), "| provider usage of the first three (not in events.jsonl):", [q.get("usage") for q in hintreq][:3])
        say("usage event kinds in log:", sorted({str(e.get("kind")) for e in ev3 if e["type"] == "usage"}))
        human = kelir("observe", sd3.name).stdout
        say("key in observe outputs:", KEY in human or KEY in rawat, "| 'authorization' in output:", "authorization" in (human + rawat).lower())
        say("key in events.jsonl:", KEY in (sd3 / "events.jsonl").read_text(encoding="utf-8"))
        say("observe human head:\n" + "\n".join(human.splitlines()[:16]))
        kelir("ctl", sd3.name, "end")
        p.wait(timeout=30)
    finally:
        if p.poll() is None:
            subprocess.run(["taskkill", "/F", "/T", "/PID", str(p.pid)], capture_output=True)
finally:
    proxy.terminate()
