#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyte", "pillow", "pywinpty; sys_platform == 'win32'"]
# ///
"""One review scenario end to end: mock server -> kelir in a pty -> raw capture -> PNGs.

usage: tui2_scn.py <name> <spec.json> [--light]
spec: {"turns":[...server turns...], "steps":[...driver steps...], "rows":40, "cols":100,
       "env":{..}, "unset":[..], "exe":"path", "settle":0.5, "cfg":"extra config.toml text", "files":{"rel":"text"},
       "binfiles":{"rel":"base64"}}
Output under $TUI2_OUT (default: a tui2-review folder in the system temp dir): img/<name>.<snap>.png, <name>.raw/json,
<name>.requests.jsonl (what the mock received), <name>.work (cwd) and <name>.state.
Starts one mock and one kelir; both are killed by exact pid at the end."""
import base64, json, os, shutil, socket, subprocess, sys, time

name, specp = sys.argv[1], sys.argv[2]
spec = json.load(open(specp, encoding="utf-8"))
here = os.path.dirname(os.path.abspath(__file__))
import tempfile
out = os.environ.get("TUI2_OUT", os.path.join(tempfile.gettempdir(), "tui2-review"))
os.makedirs(out, exist_ok=True)
root = os.path.dirname(os.path.dirname(here))
exe = spec.get("exe", os.environ.get("KELIR_EXE", os.path.join(root, "target", "release", "kelir.exe" if os.name == "nt" else "kelir")))
work = os.path.join(out, name + ".work")
state = os.path.join(out, name + ".state")
for d in (work, state):
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
for rel, text in spec.get("files", {}).items():
    p = os.path.join(work, rel)
    os.makedirs(os.path.dirname(p), exist_ok=True)
    open(p, "w", encoding="utf-8").write(text)
for rel, b64 in spec.get("binfiles", {}).items():
    p = os.path.join(work, rel)
    os.makedirs(os.path.dirname(p), exist_ok=True)
    open(p, "wb").write(base64.b64decode(b64))
cfgp = os.path.join(out, name + ".config.toml")
open(cfgp, "w").write('provider = "openrouter"\nmodel = "test/model"\n' + spec.get("cfg", ""))
scn = os.path.join(out, name + ".scenario.json")
json.dump(spec["turns"], open(scn, "w"))
log = os.path.join(out, name + ".requests.jsonl")
if os.path.exists(log):
    os.remove(log)
live = bool(spec.get("live"))  # real provider: the key comes from the pane's environment, never from here
srv = None
env = {"KELIR_CONFIG": cfgp, "KELIR_CONFIG_DIR": os.path.join(state, "no-config"), "KELIR_STATE_DIR": state,
       "KELIR_HOME": os.path.join(state, "home")}
if live:
    open(cfgp, "w").write('provider = "openrouter"\nmodel = "deepseek/deepseek-v4.1-flash-1017"\n' + spec.get("cfg", ""))
    env["PATH"] = os.path.dirname(exe) + os.pathsep + os.environ.get("PATH", "")
else:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    srv = subprocess.Popen([sys.executable, os.path.join(here, "tui2_server.py"), str(port), scn, log],
                           creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    time.sleep(0.6)
    env.update({"KELIR_BASE_URL": "http://127.0.0.1:%d" % port, "OPENROUTER_API_KEY": "test-openrouter-key"})
env.update(spec.get("env", {}))
drive = {"argv": [exe] + spec.get("args", []), "cwd": work, "env": env, "unset": spec.get("unset", []),
         "rows": spec.get("rows", 40), "cols": spec.get("cols", 100), "out": os.path.join(out, name),
         "settle": spec.get("settle", 0.5), "exit_wait": spec.get("exit_wait", 5), "steps": spec["steps"]}
dp = os.path.join(out, name + ".drive.json")
json.dump(drive, open(dp, "w"))
try:
    r = subprocess.run(["uv", "run", "--script", os.path.join(here, "tui2_drive.py"), dp], capture_output=True, text=True,
                       timeout=spec.get("timeout", 180))
    print("drive:", r.stdout.strip(), r.stderr.strip()[-500:])
finally:
    if srv:
        srv.kill()
rr = ["uv", "run", "--script", os.path.join(here, "tui2_render.py"), os.path.join(out, name), os.path.join(out, "img")]
rr += [a for a in sys.argv[3:]]
r = subprocess.run(rr, capture_output=True, text=True)
print(r.stdout.strip(), r.stderr.strip()[-800:])
