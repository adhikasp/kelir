#!/usr/bin/env python3
"""session_start / session_end gate: keep a notes repo in sync. Opt-in.

At `session_start` it runs `git pull --ff-only`; at `session_end` it commits
what changed and pushes. It never forces, never rewrites history, and never
fails the session: every error is just printed to stderr (a failed gate is
allowed anyway). It works on the repository that contains the session's cwd.

    [gates]
    session_start = "python /path/to/git_sync.py"
    session_end = { command = "python /path/to/git_sync.py", env = { GIT_SYNC_PUSH = "1" } }

What it stages is deliberately narrow:

* By default only files git already tracks (`git add -u`). A new file is left
  alone; `GIT_SYNC_ADD_NEW = "1"` also stages untracked files (still honouring
  `.gitignore`).
* It refuses to commit, and so to push, anything whose name looks like a secret
  (`.env*`, `*.pem`, `*.key`, `id_rsa*`, `id_ed25519*`, `credentials*`,
  `*secret*`, `*.p12`, `*.pfx`, `.netrc`). The index is reset and the reason
  goes to stderr; nothing is committed that run.
* `GIT_SYNC_DRY_RUN = "1"` changes nothing: it prints, on stderr, the commands
  it would run and the files it would commit.
* `GIT_SYNC_PUSH = "0"` commits but does not push.

Git needs its own credentials: `SSH_AUTH_SOCK` is passed through, nothing else.
"""
import fnmatch
import json
import os
import subprocess
import sys

SECRET_PATTERNS = [
    ".env",
    ".env.*",
    "*.env",
    "*.pem",
    "*.key",
    "*.p12",
    "*.pfx",
    "id_rsa*",
    "id_ed25519*",
    "id_ecdsa*",
    "credentials*",
    "*secret*",
    ".netrc",
]


def git(cwd: str, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, timeout=25
    )


def say(text: str) -> None:
    sys.stderr.write(text + "\n")


def secret_like(path: str) -> bool:
    """Any path component that looks like a secret's file name."""
    for part in path.replace("\\", "/").split("/"):
        name = part.lower()
        if any(fnmatch.fnmatch(name, pattern) for pattern in SECRET_PATTERNS):
            return True
    return False


def flag(name: str) -> bool:
    return os.environ.get(name, "0") not in ("", "0")


def main() -> None:
    event = json.load(sys.stdin)
    cwd = event.get("cwd", ".")
    dry = flag("GIT_SYNC_DRY_RUN")
    try:
        if git(cwd, "rev-parse", "--is-inside-work-tree").returncode != 0:
            return
        if event.get("event") == "session_start":
            if dry:
                say("git_sync (dry run): would run `git pull --ff-only`")
                done = None
            else:
                done = git(cwd, "pull", "--ff-only")
        else:
            done = sync_end(cwd, event, dry)
        if done is not None and done.returncode != 0:
            say((done.stderr or done.stdout or "git failed")[:300])
    except (OSError, subprocess.TimeoutExpired) as error:
        say(f"git_sync: {error}")
    print(json.dumps({"decision": "allow"}))


def sync_end(cwd: str, event: dict, dry: bool):
    add_new = flag("GIT_SYNC_ADD_NEW")
    push = os.environ.get("GIT_SYNC_PUSH", "1") != "0"
    if dry:
        # Nothing is staged in a dry run: read what *would* be.
        status = git(cwd, "status", "--porcelain", "--untracked-files=" + ("all" if add_new else "no"))
        names = [line[3:].strip().strip('"') for line in status.stdout.splitlines() if line.strip()]
    else:
        git(cwd, "add", "-A" if add_new else "-u")
        names = git(cwd, "diff", "--cached", "--name-only").stdout.split("\n")
        names = [name for name in names if name]
    if not names:
        return None
    risky = [name for name in names if secret_like(name)]
    if risky:
        if not dry:
            git(cwd, "reset", "-q")
        say("git_sync: not committing, secret-like file(s): " + ", ".join(risky[:5]))
        return None
    if dry:
        say("git_sync (dry run): would commit " + ", ".join(names[:20]))
        say("git_sync (dry run): would " + ("push" if push else "not push"))
        return None
    done = git(cwd, "commit", "-m", "kelir: " + str(event.get("session", "session")))
    if done.returncode == 0 and push:
        done = git(cwd, "push")
    return done


if __name__ == "__main__":
    main()
