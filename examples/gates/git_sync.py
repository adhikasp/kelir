#!/usr/bin/env python3
"""session_start / session_end gate: keep a notes repo in sync. Opt-in.

At `session_start` it runs `git pull --ff-only`; at `session_end` it commits
what changed and pushes. It never forces, never rewrites history, and never
fails the session: every error is just printed to stderr (a failed gate is
allowed anyway). It only touches the repository at the session's cwd.

    [gates]
    session_start = "python /path/to/git_sync.py"
    session_end = { command = "python /path/to/git_sync.py", env = { GIT_SYNC_PUSH = "1" } }

`GIT_SYNC_PUSH = "0"` commits but does not push. Git needs its own
credentials: `SSH_AUTH_SOCK` is passed through, nothing else is.
"""
import json
import os
import subprocess
import sys


def git(cwd: str, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, timeout=25
    )


def main() -> None:
    event = json.load(sys.stdin)
    cwd = event.get("cwd", ".")
    try:
        if git(cwd, "rev-parse", "--is-inside-work-tree").returncode != 0:
            return
        if event.get("event") == "session_start":
            done = git(cwd, "pull", "--ff-only")
        else:
            git(cwd, "add", "-A")
            if git(cwd, "diff", "--cached", "--quiet").returncode == 0:
                return
            done = git(cwd, "commit", "-m", "kelir: " + str(event.get("session", "session")))
            if done.returncode == 0 and os.environ.get("GIT_SYNC_PUSH", "1") != "0":
                done = git(cwd, "push")
        if done.returncode != 0:
            sys.stderr.write((done.stderr or done.stdout or "git failed")[:300] + "\n")
    except (OSError, subprocess.TimeoutExpired) as error:
        sys.stderr.write(f"git_sync: {error}\n")
    print(json.dumps({"decision": "allow"}))


if __name__ == "__main__":
    main()
