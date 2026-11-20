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
  call makes it not a read call, and so does any write: a redirect into a file, `sed -i`,
  `find -delete`, `sort -o`, `git branch -D`, `git config k v`, `sleep`. The split is quote-aware
  (a `grep 'a\|b'` is one command, a `\"` inside quotes does not end them), heredoc bodies are
  dropped, and `bash -c '…'`, `powershell -Command "…"` and `for … do … done` are looked into;
- **model minutes**: sum of `assistant.ts - step_started.ts`;
- **tool minutes**: the union of `tool_started.ts -> tool_settled.ts`, so parallel calls are not
  counted twice;
- **big steps**: steps with at least `--big-tokens` (default 2000) completion tokens; the brief's
  271/180 counts are the >= 1000 population;
- **big -> cheap**: of those, the steps whose every call was a read call — the re-orientation case:
  a step that thought hard and then only looked again;
- **prompt**: with `--by-prompt`, one more table per base prompt the sessions ran (read from
  `session_started.system.text`: `steer12`, `batch-reads`, `pre-m5`, `no-batching`);
- **prompt tokens/step**, and **commits** (from `git log` in the session's cwd, inside its window;
  `--commits N` overrides).

The fixture pair under `tests/fixtures/steer12/` (`events.jsonl`, `expected.json`) pins the
arithmetic: `python scripts/metrics/trajectory.py --self-test tests/fixtures/steer12/events.jsonl
tests/fixtures/steer12/expected.json`, run by `tests/steer12.rs`.

## BEFORE — six sessions, three different prompts

The six sessions are the baseline of the harness before STEER-12, **not** of one old prompt. The
base prompt is frozen into `session_started.system.text`, and `--by-prompt` reads it, so the split
below is from the logs and not from memory:

| prompt the session ran | sessions | how it is recognised |
|---|---|---|
| `batch-reads`: the prompt STEER-12 replaced ("Batch independent reads (parallel calls work)") | `buildinfrafix` (203 of 1727 steps, 12%) | 8.6 KB system text with that line |
| `pre-m5`: a 277-char prompt ("Reach for `bash` to read, search …") | `steer1fix` (the one session), `steer3`, `steer5` | the `Reach for` line |
| `no-batching`: a 6.5 KB wording with no batching and no parallel line | `tui2`, `tui2fix` | neither line |

So "old prompt vs new prompt" can only be read from the `batch-reads` row, which is one session. The
full set is still the right baseline for "the harness before STEER-12", which is what the brief's
numbers describe.

Command (the state root is the directory that holds `<job>/sessions/<id>/events.jsonl`; it is not
repo content, so it stays a shell variable here):

```sh
S=<state root>
python scripts/metrics/trajectory.py --by-prompt --commits 0 \
  $S/buildinfrafix \
  $S/steer1fix/sessions/ses_01m7s6mkp9psgkn89tr5snmxcr \
  $S/steer3 $S/steer5 $S/tui2 $S/tui2fix
```

`steer1fix` holds two sessions and only `ses_01m7s6mkp9…` is one of the six, so its session
directory is named instead of its job. **Commits.** `--commits 0` pins the column to zero because
those worktrees are gone from disk and the script derives commits from `git log` in the session's
`cwd`; zero there means "not measured", not "no commits". They can be recovered by hand from `main`
by subject prefix inside each session window (the review counted about 55: buildinfrafix 1,
steer1fix 0, steer3 0, steer5 14, tui2 28, tui2fix 12, roughly 8.6 model minutes per commit), which
is sensitive to merges and prefixes and is an order of magnitude, so the table keeps `n/a`; pass
`--commits N` to supply a hand count.

| session | prompt | steps | calls | calls/step | read calls | model min | tool min | big steps | big->cheap | big->cheap model min |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| buildinfrafix | batch-reads | 203 | 222 | 1.09 | 58 | 104.7 | 44.0 | 43 | 1 (2%) | 0.5 |
| steer1fix | pre-m5 | 17 | 19 | 1.12 | 19 | 0.6 | 0.1 | 0 | 0 (n/a) | 0.0 |
| steer3 | pre-m5 | 33 | 43 | 1.30 | 42 | 9.0 | 0.2 | 2 | 2 (100%) | 7.6 |
| steer5 | pre-m5 | 410 | 421 | 1.03 | 192 | 120.8 | 145.3 | 39 | 26 (67%) | 51.8 |
| tui2 | no-batching | 606 | 624 | 1.03 | 211 | 103.8 | 109.8 | 38 | 9 (24%) | 12.8 |
| tui2fix | no-batching | 458 | 471 | 1.03 | 189 | 132.1 | 72.1 | 30 | 26 (87%) | 76.9 |
| **total** | | **1727** | **1800** | **1.04** | **711** | **470.9** | **371.3** | **152** | **64 (42%)** | **149.6** |

Per prompt, same columns (big = 2000 completion tokens):

| prompt | sessions | steps | calls/step | read share | model min | tool min | model share of model+tool | big | big->cheap | big->cheap share of model time |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| batch-reads (the replaced prompt) | 1 | 203 | 1.09 | 26.1% | 104.7 | 44.0 | 70.4% | 43 | 1 (2%) | 0.5% |
| pre-m5 | 3 | 460 | 1.05 | 52.4% | 130.4 | 145.5 | 47.3% | 41 | 28 (68%) | 45.5% |
| no-batching | 2 | 1064 | 1.03 | 36.5% | 235.9 | 181.9 | 56.5% | 68 | 35 (51%) | 38.0% |
| all six | 6 | 1727 | 1.04 | 39.5% | 470.9 | 371.3 | 55.9% | 152 | 64 (42%) | 31.8% |

Totals, one metric per row (the AFTER column is filled by the first implement job and the first fix
job that run on the new prompt; see "AFTER" below):

| metric | before (all six) | after |
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

What the table says: the trajectory is single-call (1.04 calls/step; only `steer3` reaches 1.30),
reads are about two of every five calls, and generation takes 56% of model+tool time while tools
take 44%. The 42% "big then only reads" share comes almost entirely from the two older prompts: the
one session on the replaced `batch-reads` prompt has 2% (1 of 43 big steps), so the target "under
35%" is not a movement the new prompt can claim against that session. Compare an AFTER set against
the row for the prompt it replaces, and against all six only as the harness-before-STEER-12 figure.

**Reproduction notes, because they differ from the brief's numbers.** The brief quotes 1,725 model
calls, 1,800 tool calls, 14.7 h and 180/271 big->cheap steps; this script on those six logs gives
1,727 steps, 1,800 tool calls, 14.18 h of session windows and 64/152 big->cheap steps. The tool-call
count matches exactly, the wall time is within the 16.67 h span of the same logs, and the step count
differs by two (`step_started` rows that never got an `assistant`, i.e. killed or failed requests —
they are steps and cost model time, so they are counted).

The big->cheap gap has two causes, and the larger is the **threshold**: the brief's 271 / 180 counts
reproduce at **>= 1000** completion tokens, not 2000 (this script: 272 big steps at 1000, 152 at
2000), so nobody should re-derive a "2k" figure from the brief. The default here is 2000 (a step
that thought for 2k+ tokens), and `--big-tokens 1000` gives the brief's population:

| floor (completion tokens) | big steps | big->cheap | share of big | share of model time |
|---|---:|---:|---:|---:|
| >= 1000 | 272 | 87 | 32.0% | 33.6% |
| >= 2000 (default) | 152 | 64 | 42.1% | 31.8% |

The classifier is the smaller cause: the brief's 74% counted only `sed/grep/cat/ls/head/tail`
words, while this one also treats a read-only `git` verb, `wc`, `find`, `awk` and `Get-Content` as
reads. It rejects what only looks like a read: any redirect into a file (`cat > f <<EOF`,
`git show X:f > /tmp/f`, `echo x >> notes`), `sed -i`, `find -delete/-exec`, `sort -o`,
`git branch -D`, `git config k v`, `git remote add`, `sleep`, and any call that also builds. It looks
inside `bash -c '…'` and `powershell -Command "…"` payloads and through `for … do … done`, and it
honours a `\"` inside a quoted string. Together with the threshold, the share is about a third of
model time under every definition tried (35% at >= 500 tokens, 30.8% at >= 3000) — never the
brief's 74%, which does not reproduce. The corrected classifier moved the read-call total by zero
(711: `buildinfrafix` 56 -> 58 and `tui2fix` 191 -> 189 cancel) and the per-prompt split by two
calls; an independent recount by the reviewer gave 708 under a different quote handling.

## The change

The text STEER-12 replaced (the `batch-reads` prompt, the one `buildinfrafix` ran):

```
- Plan once, briefly, then act; do not redesign after each tool result.
- Batch independent reads in one call set (parallel calls work); `sed -n a,bp`
  or `git grep`, never `cat` a big file; skip `target/`.
- Check exit codes; never chain `patch && cargo test`. Calls start in the cwd
  above and share no shell state.
- Long output goes in a file. Verify before claiming: name the proving command.
```

**What this file used to claim, and why it was wrong.** The first version of this document said
the old "parallel calls work" line was true because `parallel_tool_calls` is sent for the default
model, and that the logs (1.04 calls per step) showed the flag "bought nothing". That reasoning had
a false premise. `src/provider.rs` sends `parallel_tool_calls` **only for OpenRouter**
(`ProviderKind::OpenRouter`, and only when the model is capable, `src/models.rs`: true for
`deepseek/`). All six BEFORE sessions are `opencode-go`, where the field is never in the request
(checked on the real binary behind a logging proxy, bodies only: opencode-go with the default model
-> absent; OpenRouter with the default model on a clean state dir -> `true` on the first request;
OpenRouter with an unknown model -> absent). So 1.04 calls per step says nothing about whether the
flag works, and on opencode-go the old line promised something the request never asked for.
Enabling the flag for opencode-go was not tried: nothing shows the gateway honours it, and the
rule below needs no flag. The new prompt makes **no parallel-calls claim**, and says nothing about
the provider.

The replacement (1020 bytes) asks for the move that works on every provider, one shell command per
read round, plus a place for the plan:

```
- `bash` runs a command; cwd above, share no shell state. `edit` (`edits`:
  several hunks, all or none; no python/sed patch, it can exit 0); `compact`;
  `kelir mcp ls` lists servers.
- Skills are **pull**: read a `SKILL.md`. Directories: {{skills_dirs}}
- Your reasoning is dropped between steps, not your messages; AGENTS.md and
  CLAUDE.md are here; `kelir view` an image.
- Plan once; real thinking goes to `.kelir/notes.md` (file:line map,
  plan) via `edit`, uncommitted.
- One `bash` call per read round: `git grep`/`sed -n`/`grep` joined with `;`,
  each labelled `echo === name`; three reads in a row is one command, under
  10 KB (`head`) or it spills; `grep -A/-B`, not `grep -n` then `sed -n`;
  no big `cat`; skip `target/`.
- Never batch a build/test with reads; check exit codes; never chain `patch &&
  cargo test`; long output to a file; name the proving command.
```

Four rules carry the change:

- **One `bash` call per read round.** Shell-level batching, labelled so the sections can be told
  apart in the output.
- **An output budget: under 10 KB.** Without one the batching rule collides with the 10 KB spill
  limit: over it the model sees only the first and last 1 KB, and the review measured three
  consecutive reads summing past 10 KB in 58% of windows on the real logs. The old `<8 KB` wording
  was ambiguous (command size or output size) and obsolete (on Windows a command travels as a
  file). Two changes answer it. The prompt states the budget (`head`, ranged `sed`). And the spill
  notice now keeps the batch legible when the budget is missed: it adds a line listing every
  `=== label` section with the byte offset it starts at, built as the output streams (see
  `docs/DEVIATIONS.md`, steer12fix). For a 40 KB batch of four sections (spec, deviations, grep,
  tail; 10 KB each) the model now sees the first KB (`=== spec` and its text), the last KB, and
  `spec@0, deviations@10010, grep@20026, tail@30036`: which sections exist and where, so it can
  `tail -c +10011 out | head -c 4000` the missing ones instead of mistaking the visible tail for
  the whole answer. An empty section (a `grep` with no match) is still indistinguishable from a
  cut one in the visible text, but the offsets show it (two labels at the same offset gap of a
  few bytes).
- **Notes go to `.kelir/notes.md`.** The old text said "a notes file" and named no place, so the
  default was the repo root, where `git add -A` would commit it. `.kelir/notes.md` is git-ignored
  in this repository (`.gitignore`) and the prompt says "uncommitted", so a repository that does not
  ignore it still gets a rule to leave it out of commits. (`.kelir/prompt.md` and `.kelir/skills/`
  are tracked project files, so only the one file is ignored.)
- **The guardrails kept from the old text:** never batch a build or a test with the reads, exit
  codes still checked, the proving command named. A batch's exit code is the last command's, so
  a failed read in the middle is visible only as text (`sed: can't read …`): the labels are what
  make it visible.

The prompt stays under 1 KB (1020 bytes), pinned byte-for-byte by
`tests/fixtures/prompt.md.snapshot` and line-by-line by the retro rule list in `src/sources.rs`.

## A/B on the final prompt (steer12fix)

The same small A/B the reviewer ran, repeated on the final wording (1020 bytes, `.kelir/notes.md`,
10 KB budget, no parallel claim). OLD = `git show b7b60c6:src/prompt.md` (the `batch-reads` prompt) via
config `prompt =`; NEW = the embedded prompt of the release binary built from this branch.
`deepseek/deepseek-v4.1-flash-1017` on OpenRouter (the cheap model, `parallel_tool_calls` sent in
both arms), a local clone of the repo as cwd, a fresh state dir per run, two read-only questions
(T1 "how does the gate lock work", T2 "where are provider retries decided"), 3 runs per cell. Scripts:
`scripts/live/steer12_ab.sh` and `steer12_abstats.py` (unchanged from the review's run; the OLD arm's prompt path is passed with forward slashes, since
a Windows path with backslashes is not valid inside the TOML string).

| task / arm | steps (3 runs) | tool calls | calls = reads | steps with 2 calls | prompt tokens (sum) | completion tokens (sum) |
|---|---|---:|---:|---:|---:|---:|
| T1 gate lock, OLD | 3, 4, 4 | 12 | 12 | 4 | 63,833 | 1,449 |
| T1 gate lock, NEW | 3, 3, 3 | 6 | 6 | 0 | 41,569 | 1,352 |
| T2 retries, OLD | 4, 5, 6 | 17 | 17 | 5 | 96,160 | 1,763 |
| T2 retries, NEW | 7, 6, 4 | 14 | 14 | 0 | 116,170 | 2,072 |
| all, OLD | 26 | 29 | 29 | 9 | 159,993 | 3,212 |
| all, NEW | 26 | 20 | 20 | 0 | 157,739 | 3,424 |

Reading, honestly: **tool calls (all reads) fall 31%** (29 -> 20) and the old arm's parallel
two-call steps (9 of 26) disappear, because the new prompt asks for the move that is one command
and the model does it (multi-command batches in 17 of 20 new calls vs 5 of 29 old ones). **Steps do not
fall** (26 vs 26): T1 drops (11 -> 9), T2 rises (15 -> 17), and **prompt tokens and completion
tokens are flat** (-1% and +7% in total; T2 NEW is higher than T2 OLD on both). So on these two
tasks the new prompt buys fewer calls inside the same number of steps, not a shorter trajectory;
the reviewer's earlier run (steps -12%, prompt tokens -21%, on the 1018-byte wording) is within
the noise of this one, and neither is significant: n = 3 per cell, 2-7 steps per run, one cheap
model. The tasks are too short to show the trajectory-scale effect the brief is after (fewer big
re-orientation steps), which only the AFTER sessions below can. One NEW run spilled (T2 run 2, a
three-section batch of 13.6 KB): the notice carried `provider@0, retry@4540, session@9437`, the
model followed with narrower ranged reads, and answered; no OLD run spilled. No run wrote a notes
file or an `edit` (read-only tasks), so the notes location is covered by the unit-level
`.gitignore` entry and the reviewer's live check, not by this A/B.

## AFTER — to be filled by the first two jobs on the new prompt

The brief asks for the same numbers over the next one implement job and one fix job, so the table
above can be read as a before/after pair:

```sh
S=<state root>
python scripts/metrics/trajectory.py --by-prompt $S/<implement job> $S/<fix job>   # commits derived from git
```

Expected movement, from the brief: read calls down 30-50%, big->cheap share from ~42% to under
35%, gate outcomes the same or better. If it does not move, the next lever is either a notes-file
helper (`kelir notes` — a verb that appends to the session's notes file in one call) or a repo-map
command (`kelir map` — symbols and line ranges for the files a task names), not another prompt
edit. A job that runs on the new prompt fills the AFTER column here and adds the verdict to
`docs/PROGRESS.md`.

**The six sessions above are not an AFTER.** None of them ran this prompt (see the table of prompts
above); compare an AFTER set with the `batch-reads` row for the prompt it replaced, and with all six
only as the before-STEER-12 harness. The base prompt is frozen at
`session_started`, so a session that started before this change keeps the old text for its whole
life.
