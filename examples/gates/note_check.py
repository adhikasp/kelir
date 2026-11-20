#!/usr/bin/env python3
"""after_tool gate on `edit`: run a checker over the file that was just edited.

The checker is a command you name in the gate's own `env` table (a gate does not
inherit your environment). It is run with the edited path appended; its output
becomes a note on the edit's result, so the model sees the problem at once.

    [gates]
    after_tool = { command = "python /path/to/note_check.py", tools = ["edit"], env = { NOTE_CHECK_CMD = "markdownlint" } }

No `NOTE_CHECK_CMD`, no path, a checker that is missing or times out: allow,
silently. The checker must never be able to wedge the session.
"""
import json
import os
import shlex
import subprocess
import sys

LIMIT = 1500


def allow() -> None:
    print(json.dumps({"decision": "allow"}))


def main() -> None:
    event = json.load(sys.stdin)
    checker = os.environ.get("NOTE_CHECK_CMD", "").strip()
    path = (event.get("input") or {}).get("path")
    if not checker or not path or event.get("outcome") != "ok":
        return allow()
    full = path if os.path.isabs(path) else os.path.join(event.get("cwd", "."), path)
    try:
        done = subprocess.run(
            shlex.split(checker, posix=os.name != "nt") + [full],
            capture_output=True,
            text=True,
            timeout=20,
        )
    except (OSError, subprocess.TimeoutExpired):
        return allow()
    text = ((done.stdout or "") + (done.stderr or "")).strip()
    if done.returncode == 0 and not text:
        return allow()
    note = f"the checker `{checker}` says (exit {done.returncode}):\n{text[:LIMIT]}"
    print(json.dumps({"decision": "rewrite", "reason": "checker output", "input": {"note": note}}))


if __name__ == "__main__":
    main()
