---
name: delegation
description: start child agents (kelir run --child, kelir brief) for parallel or side work, with caps, foreground vs background, a good task and reading results
metadata:
  builtin: true
  body: kelir docs
---

# Delegating to children

A child is another kelir process you start from `bash`. There is no spawn tool: the
tools stay `bash`, `edit`, `compact`. A child owns its own session (its own log and
cache), runs inside your process tree (kill the parent, no orphans), and its
result comes back to you as text.

## Which one

- **`kelir brief "<question>"`**: a side question answered by a child that sees a
  *snapshot of your context*. Cheap (it reuses your cache), cannot `edit` or `compact`,
  leaves your history alone. Use it to ask "what did we decide about X?" or to get a
  second read of a long log without spending your own context.
- **`kelir run --child "<task>"`**: a worker with a fresh context. Use it when a piece
  of work is independent of the rest and you can name its end state in a few lines.
  `--model <name>` picks another model; `--background` returns at once.

Do the work yourself when it is small, or needs your full context, or is one edit.
Delegate when it is wide (many files to survey), independent (no shared state with
what you do next) or slow (a long build you do not have to watch).

## Foreground and background

```sh
kelir run --child "list every caller of parse_config and say which ignore errors"   # blocks; stdout is the result
kelir run --child --background "run the full test suite, report failures"            # returns a handle now
```

A foreground child's final answer is the bash call's stdout. A background child
settles later: a `child_settled` arrives at your next boundary and wakes you with its
result; keep working on something else meanwhile and do not poll. Children do not
inherit your shell state, so give them paths, not "the file I just opened".

## What goes in the task

A child knows nothing you have not written. Put in: the goal in one sentence, the
paths and facts it needs, what *not* to touch, and the exact shape of the answer
("reply with a bullet per file: path, one line"). Say whether it may edit. Two children
must never edit the same file.

## Caps

Depth 3 (you are 0), 10 children per agent, 16 live descendants per root. A start past a
cap exits non-zero with a message and the count: read it, wait for a background child to
finish or do the work yourself. There is no spend cap; cost per session is in `kelir ls`.

## Reading results

Treat a child's answer as a report, not proof: spot-check what matters (read the
file it says it changed, rerun its test). A failed child (`child_settled` with an
error outcome) leaves its log in its own session dir: `kelir observe <id>` shows what
it saw. `kelir ctl <id> stop` stops one; only the root session reports Herdr status.

Depth: `kelir docs sources-and-skills`, `kelir docs verbs`, `kelir docs config` (the caps).
