#!/usr/bin/env python3
"""before_tool gate: make a session read a repo's AGENTS.md before editing it.

kelir finds instruction files by walking up from the session's starting cwd.
A session that starts elsewhere (say a notes repo) but edits a code repo never
loads that repo's AGENTS.md. This gate notices the first tool call targeting
such a repo and denies it once, telling the model to read the instructions.

Enable it in the machine config (a machine gate needs no project trust):

    [gates]
    before_tool = [
      { name = "load-project-agents", tools = ["edit", "bash"],
        command = "python3 /path/to/load_project_agents.py",
        env = { PROJECT_ROOT = "/path/to/the/code/repo" } },
    ]

Config (`PROJECT_ROOT` must be set in the gate's `env` table; gates run with
a scrubbed environment and do not inherit shell variables):

* `PROJECT_ROOT` (required): the repo to watch.
* `KELIR_AGENTS_REPO` (optional): alternate name for the same value, for
  compatibility with older copies of this gate. `PROJECT_ROOT` wins.

The gate reads the session log at `session_dir/events.jsonl` (a field of the
event on stdin), so it knows both what sources loaded at session start and
whether it has fired already. It allows a call when:

* the tool is not `edit` or `bash`, or its target is outside `PROJECT_ROOT`;
* a bash command only reads the repo (`cat`, `rg`, `head`, `sed`, ...);
* the repo's AGENTS.md was among the session's initial sources; or
* this gate has already denied once in this session (deny reasons are logged).

Otherwise it denies with a short instruction to read AGENTS.md before editing.
It fails open on bad input, a missing log, or a missing AGENTS.md.
"""
import json
import os
import re
import shlex
import sys
from pathlib import Path

# Commands that only read a file are fine, including a pipeline like `ls x | head`.
READ_PATTERN = re.compile(
    r"(?:^|[;&|]\s*|\|\|\s*|&&\s*)(?:command\s+)?"
    r"(?:cat|less|more|head|tail|sed|rg|grep|awk|bat|view|nvim|vim)\b"
)


def gate_event() -> dict:
    try:
        event = json.load(sys.stdin)
    except (ValueError, OSError):
        return {}
    return event if isinstance(event, dict) else {}


def log_path(event: dict) -> Path | None:
    raw = event.get("session_dir")
    if not isinstance(raw, str) or not raw:
        return None
    return Path(raw) / "events.jsonl"


def log_text(path: Path | None) -> str:
    if path is None:
        return ""
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return ""


def sources_include_repo_instruction(text: str, agents: Path) -> bool:
    for line in text.splitlines():
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if record.get("type") != "session_started":
            continue
        for source in record.get("sources", []) or []:
            if source.get("path") == str(agents):
                return True
    return False


def targets(event: dict, repo: Path) -> bool:
    tool = event.get("tool")
    data = event.get("input")
    if not isinstance(data, dict):
        return False

    if tool == "edit":
        paths = [data.get("path"), data.get("old"), data.get("new")]
    elif tool == "bash":
        command = data.get("command")
        if not isinstance(command, str) or READ_PATTERN.search(command):
            return False
        # Quoted, backtick-quoted and bare path-like tokens.
        paths = re.findall(r"""["'`]([^"'`\n]+)["'`]|([^\s"'`;|&<>]+)""", command)
        paths = [a or b for a, b in paths]
    else:
        return False

    root = str(repo)
    return any(isinstance(p, str) and root in p for p in paths)


def main() -> None:
    event = gate_event()
    if not event:
        return  # fail open

    raw_repo = os.environ.get("PROJECT_ROOT") or os.environ.get("KELIR_AGENTS_REPO")
    if not raw_repo:
        return  # nothing to watch: fail open
    repo = Path(raw_repo).expanduser().resolve()
    agents = repo / "AGENTS.md"
    if not agents.exists():
        return

    if not targets(event, repo):
        print(json.dumps({"decision": "allow"}))
        return

    text = log_text(log_path(event))
    if not text or sources_include_repo_instruction(text, agents):
        print(json.dumps({"decision": "allow"}))
        return

    marker = f"[load-project-agents:{repo}]"
    if marker in text:
        print(json.dumps({"decision": "allow"}))
        return

    reason = (
        f"{marker} Tools in this session are touching {repo}, but its AGENTS.md "
        "was not loaded at session start (kelir walks up from the starting cwd "
        "only). Read it before editing:"
        f" `cat {shlex.quote(str(agents))}`."
    )
    print(json.dumps({"decision": "deny", "reason": reason}))


if __name__ == "__main__":
    main()
