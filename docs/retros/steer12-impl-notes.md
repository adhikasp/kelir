# STEER-12 — implementation notes

Brief: `docs/tasks/STEER-12.md` (fewer, bigger tool steps; externalised plan; a measured prompt
change). The numbers live in `docs/PROMPT.md`; this file is what the work taught. Merged `main`
first (`git merge main`: already up to date at 1652101, the brief's own commit).

## 1. The baseline first, and it is not the brief's baseline

`scripts/metrics/trajectory.py` (PEP 723 header, Python 3.9+, no dependencies — `dependencies = []`,
and `python3`/`python` runs it as-is, so no `uv` is needed) computes the four things the brief names
from `events.jsonl` alone: steps, calls/step, read calls, model vs tool time, big steps and the
big->cheap share, prompt tokens per step, and commits (from `git log` in the session's recorded
`cwd`, filtered to the session window; `--commits N` overrides it).

Over the six sessions the brief names, the script reports 1727 steps, 1800 tool calls, 14.18 h of
session windows, 711 read calls (39.5% of calls), 470.9 model minutes vs 371.3 tool minutes, and
64 of 152 big steps (42.1%) ending in nothing but reads, worth 149.6 model minutes (31.8% of model
time). The brief says 1725 steps, 1800 calls, 14.7 h and 74%. Tool calls match exactly; steps differ
by two (`step_started` without an `assistant` — a killed or failed request is still a step and still
spent model time); wall time matches the logs' own span, not the brief's rounding. The big->cheap
gap is a definition: the brief counted only six read commands, this counts read-only `git` verbs,
`wc`, `find`, `awk`, `Get-Content` as reads and rejects a call that also runs a build. Same floor
(2000 completion tokens), same direction, a smaller share. The difference is documented in
`docs/PROMPT.md` rather than smoothed away: an AFTER table has to be compared with the same script.

**The commit columns are `n/a` and stay that way.** All six sessions ran in per-job worktrees that have since been removed, so `git log` in the recorded
cwd cannot answer, and a zero would be a lie dressed as a measurement. `--commits 0` makes the
choice explicit and the per-commit columns print `n/a`.

## 2. The parallel-calls item was already true, and the prompt should not lean on it

`src/models.rs::parallel_calls` marks `deepseek/` capable, so the default model
(`deepseek/deepseek-v4.1-flash-1017`) passes the gate, and `src/provider.rs::build_request` sends
`parallel_tool_calls: true` for OpenRouter with that capability — pinned by
`parallel_tool_calls_is_asked_for_only_when_the_model_takes_it`. No code change was needed. But the
logs show 1800 calls in 1727 steps on sessions that already ran with the flag on, so a prompt line
saying "parallel calls work" bought nothing: the model serialises anyway. The prompt now asks for
the move that does not need the capability — **one bash call carrying every read, joined with `;`
and labelled `echo === name`** — and mentions parallel calls nowhere. `tests/steer12.rs` asserts
that it does not come back.

## 3. The prompt (1015 bytes)

`src/prompt.md` moved from "batch independent reads in one call set" to three rules: one `bash` call
per read round (the shell-level batch, with `grep -A/-B` instead of `grep -n` then `sed -n`); plan
once, briefly, with real thinking writing its plan and file:line map to a **notes file** with `edit`
first, so later steps read the notes instead of the source again; and the guardrails (batch reads
but never a build or a test, exit codes still checked, commands under 8 KB, the proving command
named). Everything the retro rule list pinned before is still there except the parallel-call line,
which was replaced deliberately:

- the byte snapshot `tests/fixtures/prompt.md.snapshot` is rewritten in the same commit (the test
  says so, with both byte counts, because a changed byte moves the cache for every session);
- the rule list in `src/sources.rs::the_embedded_prompt_carries_every_retro_rule` swaps
  `Batch independent reads` for `One \`bash\` call per read round`, `echo === name`, `grep -A/-B`
  and `notes file`, each with the finding it answers.

The brief's three prose bullets did not fit 1 KB alongside the existing ones; the compression is a
trade, not an accident — the rules that survive are the ones a retro put there, and the ones that
grew a word (`never \`cat\` a big file` -> `never \`cat\` a big file; no \`target/\``) kept their
pin.

**The skills line keeps the word `Directories:`.** The first draft shortened it to `Dirs:` and the
gate caught it on both platforms: `tests/review_steer6.rs::f1_the_system_message_is_deterministic_
and_the_prefix_is_stable` cuts the live system message at the literal `Directories:` to compare the
head across two runs. The harness depends on that word, not on the sentence around it, so a prompt
edit that renames it fails a reviewer's test from another milestone — which is the rule list doing
its job one level out. Reverting one word (and one `when done` -> `what you did` to stay at 1015
bytes) was the whole fix; `src/prompt.md` now pins `Directories:` through that test as well.

## 4. The metrics script's own defects, found by running it

The first version was wrong in ways only the real logs showed; each is fixed and the fixture covers
the ones that can be pinned:

- an undefined `step_count` in the table, which crashed on the second session;
- compaction requests (`usage` rows carrying a `kind`) counted as steps and as a step's tokens;
- the commit total summed a mix of known and unknown values;
- `tool_calls_per_commit` was missing from the totals;
- the "big steps" line was labelled with the wrong noun;
- a read-call classifier that counted `echo`/`cd` as commands, so a labelled batch was not a read
  call — the exact shape the new prompt asks for;
- a plain regex split on `;|&&|\|\||\|`, which cut a `grep -n 'a\|b'` in half and called the halves
  two commands. `split_commands`/`split_words` now walk the line and respect quotes; the fixture's
  step 4 is that case, and `a_quoted_pipe_inside_grep_is_still_one_read_call` pins it (5 read calls
  of 6, not 4).

The fixture (`tests/fixtures/steer12/events.jsonl`, 30 events, plus `expected.json`) is small enough
to check by hand: one batched read, one two-read parallel step that is big and cheap, one big step
ending in a build (not cheap), one quoted-pipe read, and a text stop; a compaction `usage` rides
along to prove it is not counted. `tests/steer12.rs` runs the script's `--self-test` against it.

## 5. Tests, gates, and what is not done

`cargo test --test steer12` (5 tests) runs the script twice, checks the read classifier, the shipped
prompt and `docs/PROMPT.md`'s before-table and command. The script is skipped, with a line on
stderr, on a machine with no Python 3.9+, because it is a measurement tool and not a runtime
dependency of the product.

**The AFTER table is not filled here, on purpose.** The base prompt is rendered once at
`session_started` and frozen for the session's life, so no session that ran before this commit can
show the change. The brief asks for the next one implement job and one fix job; `docs/PROMPT.md` has
the empty column, the same command to fill it, and the next lever if it does not move (a
`kelir notes` helper or a `kelir map` command — not another prompt edit). A reviewer should treat
"AFTER is empty" as the expected state of this branch, and the claim to verify is that the BEFORE
table reproduces from the command that is written next to it.

## 6. Merging main back in (the base-prompt conflict)

`git merge main` after TUI v2, STEER-1 fixes and BUILD-INFRA landed conflicted in exactly the three
files the brief named. `docs/PROGRESS.md` was a plain union: main's three landed rows plus this
branch's STEER-12 row, main's first because they landed while this branch was in review.

The prompt was the real work and the budget was the constraint. Main's side added four things that
branch had dropped when it rewrote the text — the `kelir view` line, `calls start in the cwd above
and share no shell state`, `Long output goes in a file`, and "your messages are not" — and had to
keep its own STEER-12 rules, `grep -A/-B`, the notes file, `echo === name`, `three reads in a row
is one command`, `never a build/test`, `never chain`, `8 KB`, the pinned `Directories:` and the
byte snapshot. Both full texts together are ~1,500 bytes; the ceiling is 1,024 and two other tests
(`src/sources.rs::the_base_prompt_is_byte_identical_to_its_snapshot` and
`tests/steer12.rs::the_base_prompt_carries_the_batching_and_the_notes_file`) fail on a byte that
moves without the snapshot, so the merge had to be a re-tightening, not a concatenation.

It is 1018 bytes, and every rule from both sides is in it:

- main's behaviours kept verbatim as substrings, in main's own words where they fit: `cwd`,
  `share no shell state`, `several hunks`, `kelir mcp ls`, `kelir view`, the `Directories:` cut
  point, `check exit codes`, `never chain`, `long output to a file`, `name the proving command`;
- branch's STEER-12 rules kept: `One \`bash\` call per read round`, `each labelled \`echo ===
  name\``, `three reads in a row is one command`, `grep -A/-B` (with main's `not \`grep -n\` then
  \`sed -n\``), `never a build/test`, the `notes file` + `file:line map` + `\`edit\` first` plan
  line, `8 KB`, `never \`cat\` a big file`, `target/`;
- the parallel-call clause stayed out: `tests/steer12.rs` pins its absence and the logs in §2 are
  why.

Two wording compromises are visible and deliberate. `calls start in the cwd above` became `cwd
above` as a clause on the `bash` bullet rather than its own sentence, because the sentence cost 40
bytes the plan line needed. "Long output goes in a file. Verify before claiming:" became `long
output to a file` and `name the proving command` in the guardrail — the same two behaviours, one
clause shorter, and `verify` survives in the opening line. Every other line is main's or the
branch's as written.

The measurement this branch exists for is unaffected: the notes-file and one-bash-call-per-read
rules are byte-identical to what the branch shipped, so the AFTER table will be comparable with
this one. `docs/PROMPT.md` quotes the merged prompt and its new size.
