---
name: delegation
description: start another kelir process (kelir run, kelir ask, kelir bg run) for side or parallel work, with a good task and reading results
metadata:
  builtin: true
  body: kelir docs
---

# Delegating to another kelir

A child is another `kelir` process you start from `bash`. There is no spawn tool: the
tools stay `bash`, `edit`, `compact`. A child is a **fresh session**: its own log, its
own context, no memory of yours. It runs under your bash call, so it ends with the call
(or with your session, for a background job).

What exists today is plain `kelir run`, `kelir ask` and `kelir bg`. There are no depth
or count caps and no snapshot of your context: you hold the limits yourself (below).

## Which one

- **`kelir ask "<question>"`**: one question, the final text on stdout. Use it for a
  second read of something you can name in a sentence ("what does `parse_config` do on an
  empty file?") without spending your own context.
- **`kelir run "<task>"`**: a worker. It blocks until the turn ends; stdout is its
  output, exit code 0 if it finished. `--model <name>` picks another model, `--cwd <dir>`
  where it works, `--json` streams its events as NDJSON.

Do the work yourself when it is small, needs your full context, or is one edit.
Delegate when it is wide (many files to survey), independent (no shared state with what
you do next) or slow (a long build you do not have to watch).

## Foreground and background

```sh
kelir run "list every caller of parse_config and say which ignore errors"   # blocks
kelir bg run kelir run "run the full test suite, report failures"            # returns a job id now
kelir bg wait <id>        # until it settles       kelir bg out <id>   # its output
```

A background job settles later and the harness wakes you with its result at your next
boundary: keep working on something else and do not poll. Children do not inherit your
shell state, so give them paths, not "the file I just opened".

## What goes in the task

A child knows nothing you have not written. Put in: the goal in one sentence, the paths
and facts it needs, what *not* to touch, and the exact shape of the answer ("reply with a
bullet per file: path, one line"). Say whether it may edit. Two children must never edit
the same file.

## Limits you keep

Depth 1: a child you start does not start children of its own, so say so in its task.
No more than three running at once. `kelir bg cancel <id>` ends one and its tree. Each
child spends tokens of its own; there is no spend cap.

## Reading results

Treat a child's answer as a report, not proof: spot-check what matters (read the file it
says it changed, rerun its test). A failed child leaves its log in the state dir
(`kelir docs files`); `kelir observe <session>` shows what a session saw.

Depth: `kelir docs cli`, `kelir docs verbs`, `kelir docs sources-and-skills`.
