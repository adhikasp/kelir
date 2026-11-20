#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pywinpty; sys_platform == 'win32'", "psutil"]
# ///
"""P1 on the real release binary: stream N KB of mixed markdown from the scripted mock into `kelir tui` in a pty,
time send -> end marker on the wire, sample the process working set. usage: tui2c_perf.py <kb> [<kb>...]
env KELIR_EXE overrides the binary. Kills its mock and its kelir by exact pid."""
import json, os, socket, subprocess, sys, tempfile, threading, time
import psutil

here = os.path.dirname(os.path.abspath(__file__))
root = os.path.dirname(os.path.dirname(here))
exe = os.environ.get("KELIR_EXE", os.path.join(root, "target", "release", "kelir.exe" if os.name == "nt" else "kelir"))
IS_WIN = os.name == "nt"


def md(kb):
    parts, i, size = [], 0, 0
    while size < kb * 1024:
        blk = [
            "## Section %d\n\n" % i,
            "A paragraph with **bold**, *italic*, `code` and a [link](http://example.com/%d) that is long enough to wrap around a narrow pane more than once, number %d.\n\n" % (i, i),
            "- item one %d\n- item two with `code`\n  - nested a\n  - nested b\n- item three\n\n" % i,
            "```rust\nfn f%d() {\n    let x = %d;\n    println!(\"{}\", x);\n}\n```\n\n" % (i, i),
            "| Name | Value | Note |\n|---|---|---|\n| a%d | %d | first |\n| b | 2 | second row |\n\n" % (i, i),
            "> quoted line %d\n\n" % i,
        ]
        s = "".join(blk)
        parts.append(s)
        size += len(s)
        i += 1
    return "".join(parts) + "\nENDMARKZ\n"


def run(kb, cols=100, rows=40):
    work = tempfile.mkdtemp(prefix="tui2c-perf-")
    state = os.path.join(work, "state")
    os.makedirs(state)
    cfgp = os.path.join(work, "config.toml")
    open(cfgp, "w").write('provider = "openrouter"\nmodel = "test/model"\n')
    scn = os.path.join(work, "scn.json")
    json.dump([[{"text": md(kb), "chunk": int(os.environ.get("CHUNK", "4096")), "delay": 0}]], open(scn, "w"))
    s = socket.socket(); s.bind(("127.0.0.1", 0)); port = s.getsockname()[1]; s.close()
    srv = subprocess.Popen([sys.executable, os.path.join(here, "tui2_server.py"), str(port), scn, os.path.join(work, "req.jsonl")],
                           creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    time.sleep(0.6)
    env = dict(os.environ)
    env.update({"KELIR_CONFIG": cfgp, "KELIR_CONFIG_DIR": os.path.join(state, "nc"), "KELIR_STATE_DIR": state, "KELIR_HOME": os.path.join(state, "home"),
                "KELIR_BASE_URL": "http://127.0.0.1:%d" % port, "OPENROUTER_API_KEY": "test-openrouter-key", "TERM": "xterm-256color"})
    from winpty import PtyProcess
    proc = PtyProcess.spawn([exe], cwd=work, env=env, dimensions=(rows, cols))
    buf = bytearray(); total = [0]; t_end = [None]; stop = [False]; peak = [0]
    def reader():
        while not stop[0]:
            try:
                d = proc.read(65536)
            except Exception:
                break
            if not d: continue
            b = d.encode("utf-8", "surrogateescape")
            total[0] += len(b)
            buf.extend(b)
            if len(buf) > 400000: del buf[:len(buf) - 1000]
            n = b.count(b"\x1b[6n")
            if n: proc.write("\x1b[1;1R" * n)
            if t_end[0] is None and b"ENDMARKZ" in bytes(buf):
                t_end[0] = time.time()
    threading.Thread(target=reader, daemon=True).start()
    def sampler():
        try:
            p = psutil.Process(proc.pid)
            while not stop[0]:
                try:
                    peak[0] = max(peak[0], p.memory_info().peak_wset if hasattr(p.memory_info(), "peak_wset") else p.memory_info().rss)
                except Exception: pass
                time.sleep(0.05)
        except Exception: pass
    threading.Thread(target=sampler, daemon=True).start()
    t0 = None
    try:
        time.sleep(2.0)
        t0 = time.time()
        proc.write("go\r")
        deadline = t0 + 600
        while t_end[0] is None and time.time() < deadline and proc.isalive():
            time.sleep(0.05)
        dt = (t_end[0] - t0) if t_end[0] else None
        time.sleep(1.0)
        pid = proc.pid
        try:
            pp = psutil.Process(pid); rss_after = pp.memory_info().rss; ct = pp.cpu_times(); cpu = ct.user + ct.system
        except Exception: rss_after = 0; cpu = -1
        return dict(kb=kb, cols=cols, secs=None if dt is None else round(dt, 2), peak_mb=round(peak[0] / 1e6, 1), rss_after_mb=round(rss_after / 1e6, 1), wire_kb=total[0] // 1024, kelir_cpu_s=round(cpu, 2))
    finally:
        stop[0] = True
        try: proc.terminate(force=True)
        except Exception: pass
        srv.kill()


if __name__ == "__main__":
    cols = int(os.environ.get("COLS", "100"))
    for kb in sys.argv[1:]:
        print(json.dumps(run(int(kb), cols)), flush=True)
