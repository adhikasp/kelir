---
name: delegation
description: start another kelir session for side or parallel work (kelir run --child, kelir brief, kelir ls, kelir kill), with a good task and reading results
metadata:
  builtin: true
  body: kelir docs
---

# Delegating to another kelir

A child is another kelir **session** you start from `bash`. There is no spawn tool: the
tools stay `bash`, `edit`, `compact`. A child is a fresh session: its own log, its own
context, no memory of yours. Starting one from a tool call makes it your child (it shows
in `kelir ls`, it is capped, it ends with you).

## Which one

- **`kelir brief "<question>"`**: a side question on a snapshot of *your* session, so it
  knows what you know and hits your prompt cache. The answer is on stdout. It cannot edit
  or start work. Use it for "given what we found, which file is the odd one out?".
- **`kelir run --child "<task>"`**: a worker with a fresh context. It blocks until the
  turn ends; its final text is stdout, exit 0 when it finished. `--model <name>`,
  `--provider <name>`, `--cwd <dir>` choose model and place.
- **`kelir run --ask "<question>"`** and plain **`kelir run "<task>"`** start an unrelated
  session (no tree, no caps): use them only when you do not want a child.

Do the work yourself when it is small, needs your full context, or is one edit.
Delegate when it is wide (many files to survey), independent (no shared state with what
you do next) or slow (a long build you do not have to watch).

## Foreground and background

```sh
kelir run --child "list every caller of parse_config and say which ignore errors"
kelir run --child --background "run the full test suite, report failures"   # handle now
kelir ls                      # the tree: state, cache hits, task
kelir kill <session>          # stop one child and everything below it
```

A background child settles later and wakes you with its answer at your next boundary:
keep working on something else and do not poll. A parent that ends first takes its
running children with it, so wait for what you need. Children do not inherit your shell
state: give them paths, not "the file I just opened".

## What goes in the task

A child knows nothing you have not written. Put in: the goal in one sentence, the paths
and facts it needs, what *not* to touch, and the exact shape of the answer ("reply with a
bullet per file: path, one line"). Say whether it may edit. Two children must never edit
the same file.

## Limits

A start can be refused with exit code 3 and a line naming the cap: depth (3 by default),
live children per agent (10) and live descendants of the whole tree (16). Wait for one to
finish or stop one with `kelir kill`. Each child uses tokens of its own and there is no
spend cap: `kelir ls` shows the cache-hit ratio per session and per tree.

## Reading results

Treat a child's answer as a report, not proof: spot-check what matters (read the file it
says it changed, rerun its test). A failed child leaves its log in the state dir
(`kelir docs files`); `kelir observe <session>` shows what a session saw.

Depth: `kelir docs children`, `kelir docs cli`, `kelir docs verbs`.
