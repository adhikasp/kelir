#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyte", "pywinpty; sys_platform == 'win32'"]
# ///
"""Run a program in a pty (ConPTY on Windows, openpty on Linux), feed scripted input, record the raw
byte stream and the offsets of named snapshots/resizes. The driver is the terminal: it tracks the
screen with pyte and answers cursor-position (ESC[6n) and DA (ESC[c) queries with real values.

usage: tui2_drive.py <drive.json>
drive.json: {"argv":[..], "cwd":"..", "env":{..}, "rows":40, "cols":100, "out":"prefix", "settle":0.5,
             "steps":[ {"wait":1.0} | {"send":"text, with \\r \\x03 \\x1b escapes"} | {"resize":[rows,cols]} |
                       {"wait_for":"substring","timeout":20} | {"snap":"name"} |
                       {"burst_snaps":n,"dt":0.3,"prefix":"f"} ]}
writes <out>.raw and <out>.json ({"events":[{"off","kind":"snap|resize",...}],"exit","init","status","killed"}).
"""
import json, os, sys, threading, time
import pyte

cfg = json.load(open(sys.argv[1], encoding="utf-8"))
rows, cols = cfg.get("rows", 40), cfg.get("cols", 100)
env = dict(os.environ)
env.update(cfg.get("env", {}))
for k in cfg.get("unset", []):
    env.pop(k, None)
env.setdefault("TERM", "xterm-256color")
raw = bytearray()
lock = threading.Lock()
events = []
screen = pyte.Screen(cols, rows)
stream = pyte.ByteStream(screen)
IS_WIN = sys.platform == "win32"

if IS_WIN:
    from winpty import PtyProcess
    proc = PtyProcess.spawn(cfg["argv"], cwd=cfg.get("cwd"), env=env, dimensions=(rows, cols))

    def write(b):
        proc.write(b.decode("utf-8", "surrogateescape"))

    def read():
        return proc.read(65536).encode("utf-8", "surrogateescape")

    def alive():
        return proc.isalive()

    def resize(r, c):
        proc.setwinsize(r, c)

    def code():
        return proc.exitstatus

    def kill():
        proc.terminate(force=True)
else:
    import pty, fcntl, termios, struct, subprocess, select
    mfd, sfd = pty.openpty()

    def setsz(r, c):
        fcntl.ioctl(mfd, termios.TIOCSWINSZ, struct.pack("HHHH", r, c, 0, 0))
    setsz(rows, cols)
    p = subprocess.Popen(cfg["argv"], cwd=cfg.get("cwd"), env=env, stdin=sfd, stdout=sfd, stderr=sfd,
                         start_new_session=True, close_fds=True)
    os.close(sfd)

    def write(b):
        os.write(mfd, b)

    def read():
        r, _, _ = select.select([mfd], [], [], 0.2)
        if not r:
            return b""
        try:
            return os.read(mfd, 65536)
        except OSError:
            return None

    def alive():
        return p.poll() is None

    def resize(r, c):
        setsz(r, c)

    def code():
        return p.poll()

    def kill():
        try:
            p.kill()
        except Exception:
            pass


def reader():
    while True:
        try:
            data = read()
        except Exception:
            break
        if data is None:
            break
        if not data:
            if not alive():
                break
            continue
        with lock:
            raw.extend(data)
            stream.feed(data)
            for _ in range(data.count(b"\x1b[6n")):
                write(("\x1b[%d;%dR" % (screen.cursor.y + 1, screen.cursor.x + 1)).encode())
            for _ in range(data.count(b"\x1b[c")):
                write(b"\x1b[?1;2c")


threading.Thread(target=reader, daemon=True).start()


def seen(sub):
    with lock:
        if sub.encode() in bytes(raw):
            return True
        try:
            return sub in "\n".join(screen.display)
        except IndexError:  # pyte cannot render a wide char in the last column
            return False


status = "ok"
try:
    for st in cfg["steps"]:
        if "wait" in st:
            time.sleep(st["wait"])
        elif "send" in st:
            s = st["send"]
            for a, b in (("\\r", "\r"), ("\\x03", "\x03"), ("\\x0f", "\x0f"), ("\\x1b", "\x1b"), ("\\x7f", "\x7f"),
                         ("\\x16", "\x16"), ("\\n", "\n")):
                s = s.replace(a, b)
            if st.get("slow"):
                for ch in s:
                    write(ch.encode("utf-8"))
                    time.sleep(st["slow"])
            else:
                write(s.encode("utf-8"))
        elif "resize" in st:
            r, c = st["resize"]
            resize(r, c)
            with lock:
                events.append({"off": len(raw), "kind": "resize", "rows": r, "cols": c})
                screen.resize(r, c)
        elif "wait_for" in st:
            end = time.time() + st.get("timeout", 20)
            while time.time() < end and not seen(st["wait_for"]):
                time.sleep(0.05)
            if not seen(st["wait_for"]):
                status = "timeout:" + st["wait_for"]
        elif "snap" in st:
            with lock:
                events.append({"off": len(raw), "kind": "snap", "name": st["snap"]})
        elif "burst_snaps" in st:
            for i in range(st["burst_snaps"]):
                with lock:
                    events.append({"off": len(raw), "kind": "snap", "name": "%s%02d" % (st.get("prefix", "f"), i)})
                time.sleep(st.get("dt", 0.3))
    time.sleep(cfg.get("settle", 0.5))
finally:
    end = time.time() + cfg.get("exit_wait", 5)
    while alive() and time.time() < end:
        time.sleep(0.05)
    still = alive()
    if still:
        kill()
    time.sleep(0.2)
    open(cfg["out"] + ".raw", "wb").write(bytes(raw))
    json.dump({"events": events, "exit": None if still else code(), "init": [cfg.get("rows", 40), cfg.get("cols", 100)],
               "status": status, "killed": still}, open(cfg["out"] + ".json", "w"))
    print(json.dumps({"status": status, "exit": None if still else code(), "bytes": len(raw), "killed": still}))
