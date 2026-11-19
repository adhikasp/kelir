"""Start a command from a console-less (or hidden-console) parent, wait for it, print its exit code.

Usage: python -I launch_detached.py <detached|hidden> <timeout-s> <out-file> -- <program> [args...]

detached  DETACHED_PROCESS: the child has no console at all, like a service, a GUI app's child, or a
          harness that detaches. Every console program the child starts gets a console of its own, which
          Windows Terminal (the default terminal on this box) may show as a window: that is the flash case.
hidden    CREATE_NEW_CONSOLE + SW_HIDE: what PowerShell `Start-Process -WindowStyle Hidden` does.

The child's stdout/stderr go to <out-file>. The child tree is killed (taskkill /T by its exact PID) at timeout.
"""
import subprocess, sys

mode, timeout, out = sys.argv[1], float(sys.argv[2]), sys.argv[3]
assert sys.argv[4] == "--"
argv = sys.argv[5:]
DETACHED_PROCESS, NEW_GROUP, NEW_CONSOLE = 0x00000008, 0x00000200, 0x00000010
si = None
if mode == "detached":
    flags = DETACHED_PROCESS | NEW_GROUP
else:
    flags = NEW_CONSOLE | NEW_GROUP
    si = subprocess.STARTUPINFO()
    si.dwFlags |= subprocess.STARTF_USESHOWWINDOW
    si.wShowWindow = 0  # SW_HIDE
with open(out, "wb") as f:
    p = subprocess.Popen(argv, creationflags=flags, startupinfo=si, stdin=subprocess.DEVNULL, stdout=f, stderr=subprocess.STDOUT)
    print("launched", mode, "pid", p.pid, flush=True)
    try:
        code = p.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        subprocess.run(["taskkill", "/F", "/T", "/PID", str(p.pid)], capture_output=True)
        code = "timeout"
print("exit", code, flush=True)
