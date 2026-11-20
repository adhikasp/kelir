# The base prompt: what it says and what it measured (STEER-12)

`src/prompt.md` is embedded at build time and is the head of every request's frozen prefix, so it is
also the one place where a wording change can be measured across sessions. This file holds the
numbers that argue for the current wording, the command that produced them, and the change itself.
The brief: `docs/tasks/STEER-12.md`; the reasoning and the defects found on the way:
`docs/retros/steer12-impl-notes.md`.

## The measurement

`scripts/metrics/trajectory.py` (Python 3.9+, standard library only, no `uv` needed) reads one or
more session logs and prints, per session and in total:

- **steps**: model calls (`step_started`, one request each);
- **tool calls** and **calls/step**: 1.0 is a single-call trajectory;
- **read calls**: calls whose every command only reads (`sed`, `grep`, `ls`, `cat`, `head`, `tail`,
  `rg`, `wc`, `find`, a read-only `git` verb, `Get-Content`, …). `cd`, `echo` and `VAR=value` are
  neutral, so a labelled multi-read command counts once and stays a read; one build or edit in the
  call makes it not a read call. The split is quote-aware, so a `grep 'a\|b'` is one command and not
  two;
- **model minutes**: sum of `assistant.ts - step_started.ts`;
- **tool minutes**: the union of `tool_started.ts -> tool_settled.ts`, so parallel calls are not
  counted twice;
- **big steps**: steps with at least `--big-tokens` (default 2000) completion tokens;
- **big -> cheap**: of those, the steps whose every call was a read call — the re-orientation case:
  a step that thought hard and then only looked again;
- **prompt tokens/step**, and **commits** (from `git log` in the session's cwd, inside its window;
  `--commits N` overrides).

The fixture pair under `tests/fixtures/steer12/` (`events.jsonl`, `expected.json`) pins the
arithmetic: `python scripts/metrics/trajectory.py --self-test tests/fixtures/steer12/events.jsonl
tests/fixtures/steer12/expected.json`, run by `tests/steer12.rs`.

## BEFORE — six sessions on the old prompt

Command (the state root is the directory that holds `<job>/sessions/<id>/events.jsonl`; it is not
repo content, so it stays a shell variable here):

```sh
S=<state root>
python scripts/metrics/trajectory.py \
  $S/buildinfrafix \
  $S/steer1fix/sessions/ses_01m7s6mkp9psgkn89tr5snmxcr \
  $S/steer3 $S/steer5 $S/tui2 $S/tui2fix --commits 0
```

`steer1fix` holds two sessions and only `ses_01m7s6mkp9…` is one of the six, so its session
directory is named instead of its job. `--commits 0` because those worktrees are gone from disk: the
commit column cannot be derived after the fact, and a zero is the honest value, not a measurement.

| session | steps | calls | calls/step | read calls | model min | tool min | big steps | big->cheap | big->cheap model min |
|---|---|---|---|---|---:|---:|---:|---:|---:|
| buildinfrafix | 203 | 222 | 1.09 | 56 | 104.7 | 44.0 | 43 | 1 (2%) | 0.5 |
| steer1fix | 17 | 19 | 1.12 | 19 | 0.6 | 0.1 | 0 | 0 (n/a) | 0.0 |
| steer3 | 33 | 43 | 1.30 | 42 | 9.0 | 0.2 | 2 | 2 (100%) | 7.6 |
| steer5 | 410 | 421 | 1.03 | 192 | 120.8 | 145.3 | 39 | 26 (67%) | 51.8 |
| tui2 | 606 | 624 | 1.03 | 211 | 103.8 | 109.8 | 38 | 9 (24%) | 12.8 |
| tui2fix | 458 | 471 | 1.03 | 191 | 132.1 | 72.1 | 30 | 26 (87%) | 76.9 |
| **total** | **1727** | **1800** | **1.04** | **711** | **470.9** | **371.3** | **152** | **64 (42%)** | **149.6** |

Totals, one metric per row (the AFTER column is filled by the first implement job and the first fix
job that run on the new prompt; see "AFTER" below):

| metric | before | after |
|---|---|---|
| sessions | 6 | — |
| session wall time | 14.18 h | — |
| steps | 1727 | — |
| tool calls | 1800 | — |
| calls per step | 1.04 | — |
| read calls | 711 | — |
| read share of calls | 39.5% | — |
| model minutes | 470.9 | — |
| tool minutes | 371.3 | — |
| model share of model+tool | 55.9% | — |
| big steps (>= 2000 completion tokens) | 152 | — |
| big -> cheap | 64 | — |
| big -> cheap share | 42.1% | — |
| big -> cheap model minutes | 149.6 | — |
| big -> cheap share of model time | 31.8% | — |
| prompt tokens per step | 181,590 | — |
| commits / steps per commit | n/a (worktrees gone) | — |

What the table says, in the brief's terms: the trajectory is single-call (1.04 calls/step; only
`steer3` reaches 1.30), reads are two of every five calls, and generation takes 56% of
model+tool time while tools take 44%. Four of ten big steps ended in nothing but reads — a step that
spent 2k+ completion tokens thinking and then only looked again.

**Reproduction notes, because they differ from the brief's numbers.** The brief quotes 1,725 model
calls, 1,800 tool calls, 14.7 h and 180/271 big->cheap steps; this script on those six logs gives
1,727 steps, 1,800 tool calls, 14.18 h of session windows and 64/152 big->cheap steps. The tool-call
count matches exactly, the wall time is within the 16.67 h span of the same logs, and the step count
differs by two (`step_started` rows that never got an `assistant`, i.e. killed or failed requests —
they are steps and cost model time, so they are counted). The big->cheap gap is the classifier: the
brief's 74% counted only `sed/grep/cat/ls/head/tail` words, while this one also treats a read-only
`git` verb, `wc`, `find`, `awk` and `Get-Content` as reads, and rejects a call that also runs a
build. The floor is the same 2,000 completion tokens. The direction is identical either way; the
absolute share is 42%, and that is the number the AFTER table is compared against.

## The change

The old text asked for parallel calls:

```
- Plan once, briefly, then act; do not redesign after each tool result.
- Batch independent reads in one call set (parallel calls work); `sed -n a,bp`
  or `git grep`, never `cat` a big file; skip `target/`.
- Check exit codes; never chain `patch && cargo test`. Calls start in the cwd
  above and share no shell state.
- Long output goes in a file. Verify before claiming: name the proving command.
```

`parallel_tool_calls` is in fact sent for the default model (`src/models.rs`: `parallel_calls` is
true for `deepseek/`, and `src/provider.rs` sends the flag only for OpenRouter with that capability;
pinned by `parallel_tool_calls_is_asked_for_only_when_the_model_takes_it`), so the old claim was
true — but the logs show the model still issued 1.04 calls per step, so a claim that it *may* make
two calls bought nothing. The replacement asks for the move that works regardless of the capability:

```
- One `bash` call per read round: `sed -n`/`grep`/`ls`/`git grep` joined with
  `;`, each labelled `echo === name`; three reads in a row is one command;
  `grep -A/-B`, not `grep -n` then `sed -n`; never `cat` a big file; no
  `target/`.
- Plan once, briefly; real thinking writes its plan and file:line map to a
  notes file with `edit` first; later steps read the notes, not the source.
- Batch reads, never a build/test; check exit codes; never chain `patch &&
  cargo test`; commands under 8 KB; name the proving command.
```

Three rules carry the change: one bash call per read round (the shell-level batching that needs no
provider feature), the notes file (the plan is externalised before acting, because reasoning is
dropped between steps), and the guardrails kept from the old text (never batch a build or a test
with the reads, commands under 8 KB, exit codes still checked, the proving command named). The
prompt stays under 1 KB (1015 bytes), pinned byte-for-byte by
`tests/fixtures/prompt.md.snapshot` and line-by-line by the retro rule list in `src/sources.rs`.

## AFTER — to be filled by the first two jobs on the new prompt

The brief asks for the same numbers over the next one implement job and one fix job, so the table
above can be read as a before/after pair:

```sh
S=<state root>
python scripts/metrics/trajectory.py $S/<implement job> $S/<fix job>   # commits derived from git
```

Expected movement, from the brief: read calls down 30-50%, big->cheap share from ~42% to under
35%, gate outcomes the same or better. If it does not move, the next lever is either a notes-file
helper (`kelir notes` — a verb that appends to the session's notes file in one call) or a repo-map
command (`kelir map` — symbols and line ranges for the files a task names), not another prompt
edit. A job that runs on the new prompt fills the AFTER column here and adds the verdict to
`docs/PROGRESS.md`.

**The six sessions above are not an AFTER.** They ran the old prompt; the base prompt is frozen at
`session_started`, so a session that started before this change keeps the old text for its whole
life.
