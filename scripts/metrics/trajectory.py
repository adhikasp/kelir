#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.9"
# dependencies = []
# ///
"""Measure an agent trajectory from `events.jsonl`, without reading the model's mind.

Why: "fewer, bigger tool steps" is a claim about wall time, and the log already
holds every number needed to test it — every model call (`step_started` to
`assistant`), every tool call (`tool_started` to `tool_settled`), every token
count (`usage`) and every command (`tool_started.input`). Metrics that are not
printed from this file are opinions (STEER-12).

usage:
  trajectory.py <state-root>...            # <root>/<job>/sessions/<id>/events.jsonl
  trajectory.py <session-dir|events.jsonl>...
  trajectory.py --json                     # machine-readable
  trajectory.py --self-test FIXTURE EXPECT # known numbers, exit 1 on a mismatch

options:
  --commits N     commits landed in these sessions (default: ask git, see below)
  --big-tokens N  a "big" step's completion-token floor (default: 2000)
  --by-prompt     also one table per base prompt the sessions actually ran
                  (steer12 | batch-reads | pre-m5 | no-batching), read from
                  `session_started.system.text`
  --json          one JSON object instead of the tables
  --quiet         no notes on stderr

The metrics (one row per session, then a total):

  steps            model calls (`step_started` events; one request each)
  tool_calls       tool calls issued (`tool_started` events)
  calls/step       tool calls per step: 1.0 is a single-call trajectory
  read_calls       calls that only read: every command in the call is one of
                   sed/grep/cat/ls/head/tail/rg/wc/find/Get-Content/... (or a
                   read-only `git` verb). A call that batches five reads counts
                   once, and any non-read command in the call makes it not a read
                   call, which is the rule the prompt asks the model to follow.
  model_min        sum of (assistant.ts - step_started.ts): generation time
  tool_min         union of (tool_started.ts -> tool_settled.ts), so calls that
                   ran concurrently are not counted twice
  big_steps        steps with >= --big-tokens completion tokens
  big_cheap        of those, steps whose every tool call was a read call
  big_cheap_min    model minutes spent in big_cheap steps, and their share
  prompt_tok/step  prompt tokens per step (mean)
  commits          commits landed (see below); per-commit columns are `n/a`
                   when it is unknown

Commits: the log has no commit event, so the count comes from `git log` in the
session's `cwd`, filtered to commits whose committer date is inside the session
window (start - 1 min to end + 1 min, so a commit just before the last event
still counts). `--commits N` overrides it for a session set (a fixture, or a
run whose commits are counted by hand).

`--self-test` computes the metrics for a fixture log and compares them with an
expected JSON object, field by field, exit 1 on the first mismatch. That is the
unit test: tests/fixtures/steer12/ holds the pair, and tests/steer12.rs runs it.
"""

import json
import os
import re
import subprocess
import sys
from datetime import datetime, timedelta, timezone

READ_COMMANDS = {
    "sed", "grep", "egrep", "fgrep", "rg", "ag", "ack", "awk", "gawk",
    "cat", "ls", "dir", "head", "tail", "wc", "find", "file", "stat",
    "type", "sort", "uniq", "cut", "tr", "od", "nl", "diff", "comm",
    "column", "md5sum", "sha256sum", "du", "tree", "less", "more",
    "get-content", "select-string", "get-childitem", "get-item",
    "resolve-path", "test-path", "where.exe", "findstr",
    "which", "whereis", "jq", "base64",
}
# `git` verbs that only read the repository.
READ_GIT_VERBS = {
    "grep", "log", "show", "status", "diff", "ls-files", "rev-parse",
    "blame", "cat-file", "ls-tree", "describe", "shortlog",
}
# `git branch`, `remote` and `config` read or write depending on their arguments.
GIT_BRANCH_WRITE = {
    "-d", "-D", "-m", "-M", "-c", "-C", "-f", "-u", "--delete", "--move",
    "--copy", "--force", "--set-upstream-to", "--unset-upstream", "--edit-description",
}
GIT_CONFIG_READ = {
    "--get", "--get-all", "--get-regexp", "--get-urlmatch", "--list", "-l",
    "--show-origin", "--show-scope", "--name-only",
}
GIT_REMOTE_READ = {"-v", "--verbose", "show", "get-url"}
# Shell control words that carry no command of their own.
CONTROL_WORDS = {"for", "while", "until", "case", "done", "fi", "esac", "{", "}", "(", ")", "!"}
# ...and the ones that precede the command they introduce.
CONTROL_PREFIXES = {"do", "then", "else", "elif", "if", "while", "until", "!", "{"}
SHELLS = {"bash", "sh", "zsh", "dash"}
POWERSHELL_WRITERS = {
    "set-content", "out-file", "new-item", "remove-item", "add-content",
    "move-item", "copy-item", "rename-item", "set-item", "clear-content", "tee-object",
}
# Commands that only look: they never make a call a non-read call.
NEUTRAL_COMMANDS = {
    "echo", "printf", "cd", "pushd", "popd", "pwd", "export", "unset",
    "set", "true", "false", ":", "wait", "date", "test", "[",
    "", "2>&1", "2>&2", "1>&2",
}
HEREDOC = re.compile(r"<<-?[ ]*(['\"]?)([A-Za-z_][A-Za-z0-9_]*)\1")


def strip_heredocs(command):
    """Drop heredoc bodies: `cat > f <<'EOF' ... EOF` keeps only its first line."""
    out = []
    end = None
    for line in command.split("\n"):
        if end is not None:
            if line.strip() == end:
                end = None
            continue
        out.append(line)
        match = HEREDOC.search(line)
        if match and "<<<" not in line:
            end = match.group(2)
    return "\n".join(out)


def split_commands(command):
    """Split a shell line into simple commands, honouring quotes.

    A `|` inside `grep -n "a|b"` is an argument, not a pipe, so a plain regex
    would cut a read in half and call it a non-read. This walks the line.
    """
    command = strip_heredocs(command)
    parts = []
    current = []
    quote = None
    index = 0
    while index < len(command):
        character = command[index]
        if character == "\\" and quote != "'" and index + 1 < len(command):
            current.append(command[index:index + 2])
            index += 2
            continue
        if quote:
            current.append(character)
            if character == quote:
                quote = None
            index += 1
            continue
        if character in "'\"":
            quote = character
            current.append(character)
            index += 1
            continue
        if character in ";\n":
            parts.append("".join(current))
            current = []
            index += 1
            continue
        if command.startswith("&&", index) or command.startswith("||", index):
            parts.append("".join(current))
            current = []
            index += 2
            continue
        if character == "|":
            parts.append("".join(current))
            current = []
            index += 1
            continue
        current.append(character)
        index += 1
    parts.append("".join(current))
    return [part.strip() for part in parts if part.strip()]


def split_words(part):
    """Split one simple command into words, honouring quotes."""
    words = []
    current = []
    quote = None
    skip = False
    for position, character in enumerate(part):
        if skip:
            skip = False
            current.append(character)
            continue
        if character == "\\" and quote != "'" and position + 1 < len(part):
            current.append(character)
            skip = True
            continue
        if quote:
            current.append(character)
            if character == quote:
                quote = None
            continue
        if character in "'\"":
            quote = character
            current.append(character)
            continue
        if character.isspace():
            if current:
                words.append("".join(current))
                current = []
            continue
        current.append(character)
    if current:
        words.append("".join(current))
    return words


def parse_ts(text):
    if text is None:
        return None
    if text.endswith("Z"):
        text = text[:-1] + "+00:00"
    try:
        return datetime.fromisoformat(text)
    except ValueError:
        # Trailing sub-microsecond digits (the log writes nanoseconds).
        base, _, frac = text.partition(".")
        if not frac:
            raise
        digits = re.match(r"[0-9]+", frac)
        rest = frac[digits.end():] if digits else ""
        micro = (digits.group(0) + "000000")[:6] if digits else "000000"
        return datetime.fromisoformat(f"{base}.{micro}{rest}")


def unquote(word):
    if len(word) >= 2 and word[0] == word[-1] and word[0] in "'\"":
        return word[1:-1]
    return word


REDIRECT = re.compile(r"^(?:\d*|&)>>?(.*)$")


def writes_to_file(words):
    """True when an unquoted word redirects output into a file."""
    for index, word in enumerate(words):
        if word[:1] in "'\"":
            continue
        match = REDIRECT.match(word)
        if not match:
            continue
        target = match.group(1)
        if not target and index + 1 < len(words):
            target = words[index + 1]
        target = unquote(target).lower()
        if target.startswith("&") or target in ("/dev/null", "nul", "$null"):
            continue
        return True
    return False


def segments(command):
    """The command words of a shell line, one list per simple command."""
    out = []
    for part in split_commands(command):
        if part.startswith("#"):
            continue
        words = split_words(part)
        # Drop leading `VAR=value` environment assignments and `do`/`then`/`if`.
        while words and (
            re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", words[0])
            or (words[0] in CONTROL_PREFIXES and len(words) > 1)
        ):
            words = words[1:]
        if words:
            out.append(words)
    return out


def program(word):
    name = unquote(word).replace("\\", "/").rsplit("/", 1)[-1].lower()
    for suffix in (".exe", ".cmd", ".bat", ".ps1"):
        if name.endswith(suffix):
            name = name[: -len(suffix)]
    return name


def positionals(words):
    return [word for word in words if not word.startswith("-")]


def classify_git(words):
    rest = list(words[1:])
    # Skip `-C dir` and `-c k=v` global options.
    while rest and rest[0] in ("-C", "-c") and len(rest) > 2:
        rest = rest[2:]
    rest = [word for word in rest if not word.startswith("--no-pager")]
    verb_at = next((i for i, word in enumerate(rest) if not word.startswith("-")), None)
    if verb_at is None:
        return "other"
    verb, args = rest[verb_at], rest[verb_at + 1:]
    if verb in READ_GIT_VERBS:
        return "read"
    if verb == "branch":
        flagged = {word.split("=")[0] for word in args if word.startswith("-")}
        return "other" if flagged & GIT_BRANCH_WRITE or len(positionals(args)) > 0 and not (
            flagged & {"--list", "-l", "--contains", "--merged", "--no-merged", "--points-at"}
        ) else "read"
    if verb == "config":
        if {word for word in args if word.startswith("-")} & GIT_CONFIG_READ:
            return "read"
        return "read" if len(positionals(args)) == 1 else "other"
    if verb == "remote":
        sub = positionals(args)
        return "read" if not sub or sub[0] in GIT_REMOTE_READ or "-v" in args else "other"
    return "other"


def classify_powershell(words):
    tokens = []
    for word in words[1:]:
        text = unquote(word)
        tokens.extend(split_words(text) if text != word else [word])
    inner = []
    for token in tokens:
        inner.append(program(token))
        inner.extend(program(piece) for piece in re.split(r"[;|]", unquote(token)) if piece.strip())
    if any(item in POWERSHELL_WRITERS for item in inner) or writes_to_file(tokens):
        return "other"
    return "read" if any(item in READ_COMMANDS for item in inner) else "other"


def classify(words):
    """`read`, `neutral` (a label, an env var, a no-op) or `other`."""
    name = program(words[0])
    if writes_to_file(words):
        return "other"
    if name in NEUTRAL_COMMANDS or name in CONTROL_WORDS:
        return "neutral"
    if name == "git":
        return classify_git(words)
    if name in ("powershell", "pwsh"):
        return classify_powershell(words)
    if name in SHELLS:
        for index, word in enumerate(words[1:-1], start=1):
            if word in ("-c", "-lc", "-ec"):
                return "read" if is_read_call(unquote(words[index + 1])) else "other"
        return "other"
    args = words[1:]
    if name == "sed" and any(
        word == "--in-place" or word.startswith("--in-place=")
        or (word.startswith("-") and not word.startswith("--") and "i" in word)
        for word in args
    ):
        return "other"
    if name == "find" and any(
        word in ("-delete", "-exec", "-execdir", "-ok", "-fprint", "-fprintf", "-fls")
        for word in args
    ):
        return "other"
    if name in ("sort", "awk", "gawk") and any(
        word == "-o" or word.startswith("--output") for word in args
    ):
        return "other"
    return "read" if name in READ_COMMANDS else "other"


def is_read_call(command):
    """A tool call is a read call when none of its commands does anything else.

    `sed -n ...; echo === name; grep ...` is one read call: the `echo` labels and
    the `cd` are neutral, which is what the prompt asks the model to write.
    """
    if not command or not command.strip():
        return False
    kinds = [classify(words) for words in segments(command)]
    return bool(kinds) and all(kind != "other" for kind in kinds)


def union_seconds(intervals):
    if not intervals:
        return 0.0
    intervals.sort()
    total = 0.0
    start, end = intervals[0]
    for a, b in intervals[1:]:
        if a <= end:
            end = max(end, b)
        else:
            total += (end - start).total_seconds()
            start, end = a, b
    total += (end - start).total_seconds()
    return total


class Session:
    def __init__(self, path):
        self.path = path
        self.events = []
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if not line:
                    continue
                try:
                    self.events.append(json.loads(line))
                except json.JSONDecodeError:
                    # A torn last line is legal in this log (crash mid-write).
                    continue
        self.cwd = None
        for event in self.events:
            if event.get("type") == "session_started":
                self.cwd = event.get("cwd")
                break

    def prompt_label(self):
        """Which base prompt this session ran, read from `session_started.system.text`."""
        for event in self.events:
            if event.get("type") != "session_started":
                continue
            system = event.get("system")
            text = system.get("text", "") if isinstance(system, dict) else str(system or "")
            if "One `bash` call per read round" in text:
                return "steer12"
            if "Batch independent reads" in text:
                return "batch-reads"
            if "Reach for `bash`" in text:
                return "pre-m5"
            return "no-batching" if text else "unknown"
        return "unknown"

    def of_type(self, kind):
        return [event for event in self.events if event.get("type") == kind]

    def window(self):
        stamps = [parse_ts(event.get("ts")) for event in self.events]
        stamps = [stamp for stamp in stamps if stamp is not None]
        return (min(stamps), max(stamps)) if stamps else (None, None)

    def metrics(self, big_tokens):
        steps = {}
        for event in self.of_type("step_started"):
            steps.setdefault(event.get("step"), parse_ts(event.get("ts")))
        assistants = self.of_type("assistant")
        # A `usage` with a `kind` is a compaction's own request, not a step.
        usage = {
            event.get("step"): event
            for event in self.of_type("usage")
            if not event.get("kind")
        }

        model_seconds = 0.0
        steps_with_response = set()
        for event in assistants:
            started = steps.get(event.get("step"))
            ended = parse_ts(event.get("ts"))
            if started is None or ended is None:
                continue
            steps_with_response.add(event.get("step"))
            if ended > started:
                model_seconds += (ended - started).total_seconds()

        started_at = {}
        intervals = []
        for event in self.of_type("tool_started"):
            started_at[event.get("call_id")] = parse_ts(event.get("ts"))
        for event in self.of_type("tool_settled"):
            began = started_at.get(event.get("call_id"))
            ended = parse_ts(event.get("ts"))
            if began is not None and ended is not None:
                intervals.append((began, ended))
        tool_seconds = union_seconds(intervals)

        calls = sum(len(event.get("tool_calls") or []) for event in assistants)
        commands = {}
        for event in self.of_type("tool_started"):
            if event.get("name") == "bash":
                commands[event.get("call_id")] = (event.get("input") or {}).get(
                    "command", ""
                )
        read_calls = sum(1 for command in commands.values() if is_read_call(command))
        bash_calls = len(commands)

        # A step's calls, from the assistant message (the provider's answer), so
        # a call that was skipped or disabled still counts as issued.
        big_steps = 0
        big_cheap = 0
        big_cheap_seconds = 0.0
        for event in assistants:
            step = event.get("step")
            tokens = (usage.get(step) or {}).get("completion_tokens")
            if tokens is None:
                # `usage` may carry a `kind` and land after the assistant event;
                # fall back to the completion tokens on the step's own usage.
                continue
            if tokens < big_tokens:
                continue
            big_steps += 1
            call_list = event.get("tool_calls") or []
            cheap = bool(call_list) and all(
                call.get("name") == "bash"
                and is_read_call((call.get("args") or {}).get("command", ""))
                for call in call_list
            )
            if cheap:
                big_cheap += 1
                started = steps.get(step)
                ended = parse_ts(event.get("ts"))
                if started is not None and ended is not None and ended > started:
                    big_cheap_seconds += (ended - started).total_seconds()

        prompt_tokens = sum(
            event.get("prompt_tokens") or 0 for event in usage.values()
        )
        step_count = len(steps)
        return {
            "session": os.path.basename(
                os.path.dirname(os.path.abspath(self.path))
            ),
            "path": self.path,
            "cwd": self.cwd,
            "steps": step_count,
            "responses": len(steps_with_response),
            "tool_calls": calls,
            "bash_calls": bash_calls,
            "read_calls": read_calls,
            "model_seconds": round(model_seconds, 3),
            "tool_seconds": round(tool_seconds, 3),
            "big_steps": big_steps,
            "big_cheap": big_cheap,
            "big_cheap_seconds": round(big_cheap_seconds, 3),
            "prompt_tokens": prompt_tokens,
        }

    def commits(self, slack_seconds=60):
        """Commits landed in this session's cwd inside its window, or None."""
        begin, end = self.window()
        if begin is None or not self.cwd or not os.path.isdir(self.cwd):
            return None
        begin = begin - timedelta(seconds=slack_seconds)
        end = end + timedelta(seconds=slack_seconds)
        try:
            out = subprocess.run(
                ["git", "-C", self.cwd, "log", "--format=%cI", "--all"],
                capture_output=True,
                text=True,
                timeout=20,
            )
        except (OSError, subprocess.SubprocessError):
            return None
        if out.returncode != 0:
            return None
        count = 0
        for line in out.stdout.splitlines():
            try:
                stamp = parse_ts(line.strip())
            except ValueError:
                continue
            if stamp is not None and begin <= stamp <= end:
                count += 1
        return count


def collect(arguments):
    paths = []
    for argument in arguments:
        if os.path.isfile(argument):
            paths.append(argument)
            continue
        # A session directory itself, `<root>/<job>/sessions/<id>`.
        own = os.path.join(argument, "events.jsonl")
        if os.path.isfile(own):
            paths.append(own)
            continue
        # A state root: <root>/<job>/sessions/<id>/events.jsonl
        root_sessions = os.path.join(argument, "sessions")
        if os.path.isdir(root_sessions):
            for name in sorted(os.listdir(root_sessions)):
                log = os.path.join(root_sessions, name, "events.jsonl")
                if os.path.isfile(log):
                    paths.append(log)
            continue
        found = False
        for job in sorted(os.listdir(argument)):
            log_dir = os.path.join(argument, job, "sessions")
            if not os.path.isdir(log_dir):
                continue
            for name in sorted(os.listdir(log_dir)):
                log = os.path.join(log_dir, name, "events.jsonl")
                if os.path.isfile(log):
                    paths.append(log)
                    found = True
        if not found:
            raise SystemExit(f"trajectory: nothing to read under {argument}")
    return sorted(paths)


def summarise(rows, commits, big_tokens):
    if commits is None:
        known = [row.get("commits") for row in rows]
        # The total is only meaningful when every session's count is known.
        commits = sum(known) if known and all(isinstance(n, int) for n in known) else None
    steps = sum(row["steps"] for row in rows)
    calls = sum(row["tool_calls"] for row in rows)
    reads = sum(row["read_calls"] for row in rows)
    model = sum(row["model_seconds"] for row in rows)
    tool = sum(row["tool_seconds"] for row in rows)
    big = sum(row["big_steps"] for row in rows)
    cheap = sum(row["big_cheap"] for row in rows)
    cheap_seconds = sum(row["big_cheap_seconds"] for row in rows)
    prompt = sum(row["prompt_tokens"] for row in rows)
    total = {
        "sessions": len(rows),
        "steps": steps,
        "tool_calls": calls,
        "calls_per_step": round(calls / steps, 3) if steps else None,
        "read_calls": reads,
        "bash_calls": sum(row["bash_calls"] for row in rows),
        "read_share": round(reads / calls, 3) if calls else None,
        "model_minutes": round(model / 60, 2),
        "tool_minutes": round(tool / 60, 2),
        "model_share": round(model / (model + tool), 3) if model + tool else None,
        "big_tokens": big_tokens,
        "big_steps": big,
        "big_cheap": cheap,
        "big_cheap_share": round(cheap / big, 3) if big else None,
        "big_cheap_minutes": round(cheap_seconds / 60, 2),
        "big_cheap_model_share": (
            round(cheap_seconds / model, 3) if model else None
        ),
        "prompt_tokens": prompt,
        "prompt_tokens_per_step": round(prompt / steps, 1) if steps else None,
        "commits": commits,
        "steps_per_commit": round(steps / commits, 2) if commits else None,
        "read_calls_per_commit": (
            round(reads / commits, 2) if commits else None
        ),
        "model_minutes_per_commit": (
            round(model / 60 / commits, 2) if commits else None
        ),
        "tool_calls_per_commit": (
            round(calls / commits, 2) if commits else None
        ),
    }
    return total


def table(total, rows):
    def show(value):
        if value is None:
            return "n/a"
        if isinstance(value, float):
            return f"{value:g}"
        return str(value)

    lines = []
    head = (
        f"{'session':<15} {'steps':>6} {'calls':>6} {'c/step':>7} {'reads':>6} "
        f"{'model':>7} {'tool':>7} {'big':>4} {'cheap':>5} {'cheap%':>7} "
        f"{'ptok/step':>10} {'commits':>8}"
    )
    lines.append(head)
    for row in rows:
        steps = row["steps"] or 0
        calls = row["tool_calls"] or 0
        cheap_share = (
            f"{100.0 * row['big_cheap'] / row['big_steps']:.0f}%"
            if row["big_steps"]
            else "n/a"
        )
        per_step = calls / steps if steps else 0.0
        tokens_per_step = row["prompt_tokens"] / steps if steps else 0.0
        lines.append(
            f"{row['session'][:15]:<15} {steps:=6} {calls:=6} "
            f"{per_step:=7.2f} {row['read_calls']:=6} "
            f"{row['model_seconds'] / 60:=7.1f} {row['tool_seconds'] / 60:=7.1f} "
            f"{row['big_steps']:=4} {row['big_cheap']:=5} {cheap_share:>7} "
            f"{tokens_per_step:=10.0f} {show(row.get('commits')):>8}"
        )
    return "\n".join(lines)


def render(total):
    pairs = [
        ("sessions", total["sessions"]),
        ("steps", total["steps"]),
        ("tool calls", total["tool_calls"]),
        ("calls per step", total["calls_per_step"]),
        ("read calls", total["read_calls"]),
        ("read share of calls", total["read_share"]),
        ("model minutes", total["model_minutes"]),
        ("tool minutes (union)", total["tool_minutes"]),
        ("model share of model+tool", total["model_share"]),
        ("big steps (>= tokens)", total["big_steps"]),
        ("big -> cheap steps", total["big_cheap"]),
        ("big -> cheap share", total["big_cheap_share"]),
        ("model minutes in big -> cheap", total["big_cheap_minutes"]),
        ("big -> cheap share of model time", total["big_cheap_model_share"]),
        ("prompt tokens per step", total["prompt_tokens_per_step"]),
        ("commits", total["commits"]),
        ("steps per commit", total["steps_per_commit"]),
        ("read calls per commit", total["read_calls_per_commit"]),
        ("tool calls per commit", total["tool_calls_per_commit"]),
        ("model minutes per commit", total["model_minutes_per_commit"]),
    ]
    width = max(len(name) for name, _ in pairs)
    lines = []
    for name, value in pairs:
        shown = "n/a" if value is None else f"{value:g}" if isinstance(value, float) else str(value)
        lines.append(f"{name:<{width}}  {shown}")
    return "\n".join(lines)


def self_test(fixture, expected_path, big_tokens):
    expected = json.loads(open(expected_path, encoding="utf-8").read())
    commits = expected.pop("commits", None)
    rows = [Session(fixture).metrics(big_tokens)]
    total = summarise(rows, commits, big_tokens)
    failures = []
    for key, want in expected.items():
        got = total.get(key, "<missing>")
        if isinstance(want, float) and isinstance(got, (int, float)):
            same = abs(float(got) - want) < 1e-6
        else:
            same = got == want
        if not same:
            failures.append(f"{key}: expected {want!r}, got {got!r}")
    for failure in failures:
        print(f"self-test: {failure}", file=sys.stderr)
    if failures:
        print(f"self-test: {len(failures)} mismatch(es) in {fixture}", file=sys.stderr)
        return 1
    print(f"self-test: {len(expected)} fields match in {fixture}")
    return 0


def main(argv):
    arguments = []
    commits = None
    big_tokens = 2000
    as_json = False
    by_prompt = False
    quiet = False
    selftest = None
    index = 0
    while index < len(argv):
        argument = argv[index]
        if argument == "--commits":
            commits = int(argv[index + 1])
            index += 2
        elif argument == "--big-tokens":
            big_tokens = int(argv[index + 1])
            index += 2
        elif argument == "--json":
            as_json = True
            index += 1
        elif argument == "--by-prompt":
            by_prompt = True
            index += 1
        elif argument == "--quiet":
            quiet = True
            index += 1
        elif argument == "--self-test":
            selftest = (argv[index + 1], argv[index + 2])
            index += 3
        elif argument in ("-h", "--help"):
            print(__doc__)
            return 0
        elif argument.startswith("--"):
            print(f"trajectory: unknown option {argument}", file=sys.stderr)
            return 2
        else:
            arguments.append(argument)
            index += 1

    if selftest is not None:
        return self_test(selftest[0], selftest[1], big_tokens)
    if not arguments:
        print(__doc__, file=sys.stderr)
        return 2

    rows = []
    for path in collect(arguments):
        session = Session(path)
        row = session.metrics(big_tokens)
        row["prompt"] = session.prompt_label()
        row["commits"] = commits if commits is not None else session.commits()
        rows.append(row)
    total = summarise(rows, commits, big_tokens)
    if as_json:
        print(json.dumps({"total": total, "sessions": rows}, indent=2))
        return 0
    print(table(total, rows))
    print()
    print(render(total))
    if by_prompt:
        for label in sorted({row["prompt"] for row in rows}):
            group = [row for row in rows if row["prompt"] == label]
            part = summarise(group, 0 if commits == 0 else None, big_tokens)
            print(f"\n== prompt:{label} ({len(group)} session(s)) ==")
            print(table(part, group))
            print()
            print(render(part))
    if commits is None and any(row.get("commits") is None for row in rows) and not quiet:
        print(
            "\nnote: the commit count could not be derived for every session "
            "(no cwd, no git, or no commits in the window); pass --commits N",
            file=sys.stderr,
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
