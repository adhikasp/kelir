# tui3 review (calm TUI pass on top of reviewed TUI v2), reviewer/tester

Range `b7b60c6..HEAD` on `job/tui3`. The brief named one commit (`6d3d8a4`); a second implementer commit, `627d882`
("drop the window line from the TUI banner"), landed in the worktree while I was reviewing, so both are reviewed.
Fresh Claude Code session, 2026-11-20. Release binary 7.38 MB (budget 20 MB).
Method: real release binary in a ConPTY against the scripted mock (`scripts/live/tui2_scn.py`, replay through pyte to PNG,
every image looked at), scenario specs in `scripts/live/tui3_specs.py`, app-model tests in `tests/review_tui3.rs`, and one
real-model session (Herdr pane `live-tui3`, OpenRouter `deepseek/deepseek-v4.1-flash-1017`, env file sourced inside the
pane, 3 turns, 6 steps, about $0.0005). Images are in `kelir-scratch/tui3rev/out/img` (not committed).
No `src/` change by me.

## Verdict: PASS WITH FIXES (0 blockers, 4 should-fix, 6 nits)

The design does what it set out to do: the transcript is calm, prose is the only full-weight text, a `done` turn ends on
its answer, the folded line is correct in every ordering I tried, and a failed call is never hidden. What the calm design
costs is real but small and fixable. Three things are lost that a maintainer can need, one of them (the window note)
through a commit outside the tui3 brief.

## Gates (run by me)

| gate | Windows | Linux (the maintainer) |
|---|---|---|
| `bash scripts/gate.sh --linux=the maintainer` at `f57c959` (HEAD incl. my tests) | fmt ok, clippy `-D warnings` ok, nextest **872 passed, 4 skipped**, doctest ok: `gate: PASS` | ok (84 s) |
| `scripts/remote-check.sh the maintainer kelir-verify-tui3-rev` at `6d3d8a4` | n/a | rc=0, FMT-OK, CLIPPY-OK, nextest **864 passed, 3 skipped**, DOCTEST-OK |
| first `gate.sh --linux=the maintainer` at `6d3d8a4` | nextest 864 passed, 3 skipped, `gate: PASS` | ok |

The 4th skipped test is my `#[ignore]`d F-cancel test (below), not a gate skip.

## Findings

| id | sev | where | finding |
|---|---|---|---|
| F-cancel | should-fix | `src/tools_view.rs:246-247` (`group_rows`) | The checked count is `calls.iter().filter(\|c\| !c.failed())`, and `failed()` deliberately excludes a cancelled (`outcome == "stopped"`) call, so a cancelled call is folded away and counted as a success. Scenario: calls `a` ok, `b` ok, `sleep 20` cancelled with Esc, turn ends `stopped`: the transcript reads `✓ 3 calls` then `⏹ 0 steps · 0 tok`, hiding the one call that did not finish behind a check mark. Failing test: `cargo test --test review_tui3 -- --ignored a_cancelled_call_is_not_counted_as_a_checked_call` (`tests/review_tui3.rs`, ignored so the gate stays green; panics with `["✓ 3 calls", "⏹ 0 steps · 0 tok", ""]`). Fix: count only `ok && !cancelled`, and keep a cancelled call on its own line (as with one or two calls, `stopped.stopped.png`: `$ sleep 20  ⏹ cancelled`). |
| F-edit | should-fix | `src/tools_view.rs:242-262`, `docs/reference/tui.md:53-58` | `edit` (and any non-bash call) now folds like a shell call, so a file change disappears into `✓ N calls`; tui2 always kept `edit`/`write` on their own line "because the diff is what a reader cares about", and tui3 reverses that without saying why. Scenario (`fold5f`): `echo one`, `echo two && exit 3`, `echo three`, `edit notes.md`, `false` shows `✓ 3 calls` plus the two failures; `notes.md` is named nowhere (test `an_edit_inside_a_fold_is_not_named_anywhere_in_the_transcript` pins this). The only way back is `/expand <n>`, and the user does not know the ordinal of a folded call (the line shows none). A coding agent's transcript should say which files it touched. Fix: keep `edit` lines (cheap: one dim line each) or have the fold read `✓ 4 calls · 1 edit`. |
| F-window | should-fix | `src/session.rs` (commit `627d882`, banner and `apply_list`) | The banner no longer prints `window N tokens (source)` and no longer prints the `(note)` explaining a degraded window. That note was added by STEER-1 review N4 so a person sees a 401 or an offline endpoint without reading the log. Probe (`offline`, `KELIR_BASE_URL=http://127.0.0.1:9`): the banner shows model, session, cwd and the key hint only (`offline.banner.png`); the window silently falls back to the 128 000 default, which sets the compaction threshold, and nothing on screen says so. The `window` line is also absent after a plain startup, which is fine; but a window that is **not** the provider's (`default`, override, stale cache) is exactly the case worth one line. The startup message stays in the `state` output only. Not part of the tui3 brief; the commit has no docs/DEVIATIONS entry. Fix: print the banner window line (or just the note) only when `source != api`. |
| F-dev | should-fix (docs) | `docs/DEVIATIONS.md`, `docs/design/TUI-v2.md` | STEER-10 req 1 says "on finish a one-line summary (steps, tokens, cost, time)" and req 2 says tool lines carry exit/bytes/time. tui3 removes the summary on `done`, removes cost/time everywhere and the per-call duration, but writes nothing in `docs/DEVIATIONS.md`, and `docs/design/TUI-v2.md` still describes the old facts row and summary. AGENTS.md: "if you must deviate, write the deviation and the reason". `docs/reference/tui.md` is correct. No `docs/retros/tui3-impl-notes.md` exists either. |
| N-reach | nit (tracked by STEER-8) | whole | Where steps, cost and cache ratio live now: only the `usage` events in `events.jsonl` (`{"type":"usage","step":2,"prompt_tokens":1380,"cached_tokens":1024,"cost":0.0000538}`), readable with `jq`; the TUI has no `/status` and `kelir ctl status` (STEER-8) is not landed. Nothing needed to *operate* is unreachable: the step cap is in the banner, in the paused outcome line (`⏸ 2 steps · 3.6k tok`, `paused · step cap 2 reached · say continue`) and the state row; tokens stay in the facts row; the context window percent is not shown anywhere and was not before either. But a maintainer who wants the spend of a live session today must read the log. Do not call this finished until STEER-8's `status` (and the TUI showing it on request) lands. |
| N-light | nit | `src/theme.rs` (`Theme::user`, `Color::Cyan`), editor in `src/tui.rs` | The user echo and the idle editor text are ANSI cyan (index 6, a palette colour, so a terminal theme can remap it; `NO_COLOR` emits no colour SGR at all, verified in the raw stream). On a light terminal with the Windows Terminal/VS Code cyan (`#0598BC`) the contrast is about 3.4:1 (`light/fold5f.done.png`); with the classic xterm cyan (`#00CDCD`) it is about 2:1, which is faint for the text people type. Acceptable as the user's chosen direction; a one-line note in tui.md would be fair. A long prompt is a whole paragraph in cyan (`live3.t3.png`), still scannable. |
| N-count | nit | `src/tools_view.rs:247` | The fold counts only the calls it stands for, so three calls with one failure read `✓ 2 calls` followed by the failed line (`live3.t3.png`, turn 2: the user asked for three, sees "2 calls" and one red line). Correct but arithmetic; `✓ 2 ok` or counting all calls would read better. |
| N-paused | nit | `src/tui.rs:1033` | A paused turn says the same thing three times: the final text (`turn paused after 2 steps; say continue to resume`), the outcome line (`⏸ 2 steps · 3.6k tok`) and the state row (`paused · step cap 2 reached · say continue`), with no blank row between the first two (`paused.paused.png`). Same for error (`✗ 1 step · 0 tok` under `kelir: http 401: ...`). |
| N-errrow | nit | state row at 80 columns | A long provider error truncates the state row's next action: `✗ error · http 401: provider refused: {"error":{"message":"bad key"}} · say to` (`err401.err.png`, "say to retry" cut). Pre-existing; the row has no priority order for what to drop. |
| N-rlrow | nit | `src/tui.rs:308` (`facts_for_width`) | During a rate-limit wait at 80 columns the facts row falls to the last-resort form `working · 0 tok`: no `kelir`, no model, no countdown (`rl.w1.png`; at 100 columns the full line fits). Pre-existing fallback, but the model-first trimming in the new comment does not hold for the retry line; the state row carries the countdown, so no information is lost. |
| N-fold | nit (pre-existing) | tool groups | Single calls from different steps still get a blank row between them (`paused.paused.png`: `$ echo p1` / blank / `$ echo p2`); folded runs hide it. |

## What I verified, against the brief

| requirement | result | evidence |
|---|---|---|
| facts row `kelir <phase> · model · tok` only | **ok**: no step, cache or `$`, at every width; the row never exceeds its width (8..120) | `the_facts_row_is_phase_model_tokens_at_every_width`; `fold3_*`, `live3` (`kelir idle · deepseek/deepseek-v4.1-flash-1017 · 9.5k tok`) |
| per-call duration dropped | **ok** (`ok · 3 B`, `✗ exit 2 · 68 B`) | `inter.done.png`, `live3.t3.png` |
| 3+ calls fold into one `✓ N calls`; 1-2 stay individual; failures always visible | **ok**, except F-cancel/F-edit | `fold3_100/80/60/40`, `two.done.png`, `fold5f` (failed lines plus tails survive, in order), `every_failure_in_a_fold_keeps_its_line_and_tail`, `an_all_failed_run_has_no_checkmark_line` |
| fold never hides a running call | **ok**: a running call is never in the group; the state row names it (`running bash: sleep 6 2s`, `running 4 tools 2s · bash: sleep 6`), and `✓ 3 calls` stays above while a 4th runs (`running.mid1.png`) | `running`, `running_same` |
| ordering (old B1 must not return) | **ok**: calls then answer, answer then calls, interleaved, streaming; the open group previews exactly what it commits | `ord`-style `inter.done.png` (1 call, prose, 1 call, prose, then `✓ 3 calls`, prose), `prose_and_folds_keep_event_order`, `a_streaming_delta_commits_the_fold_before_its_own_rows`, `the_open_group_preview_equals_what_commits` |
| failed call among a fold is findable | **ok**: own red line plus tail; `/expand` (last) and `/expand 2` append its output; the fold line itself cannot be expanded (documented), and shows no ordinals | `fold5f.expand2.png` |
| `done` ends on prose; paused/stopped/error keep one outcome line without cost or time | **ok** | `outcome_lines_exist_only_for_turns_that_need_a_human`, `paused.paused.png`, `err401.err.png`, `stopped.stopped.png` |
| rate-limited row, countdown | **ok** (state row `rate limited · 429 · retrying in 5 s · attempt 1 · esc stops`, scrollback line, Esc stops); facts-row fallback N-rlrow | `rl.w1.png`, `rl.stopped.png` |
| user echo and idle editor cyan; working editor dim | **ok** | every image; raw stream has only `38;5;6` for them |
| `NO_COLOR`, `KELIR_ASCII` | **ok**: no colour SGR under `NO_COLOR` (raw stream: only `0`, `2`, `22`); ASCII shows `v 3 calls`, `*`, `>` | `nocolor`, `ascii.done.png` |
| 80 / 60 / 40 columns, resize | **ok**: `✓ 3 calls` and the answer at 100/80/60/40; resize 100 to 40 to 100 repaints the viewport (the scrollback above is the terminal's) | `fold3_*`, `resize.*` |
| light terminal | legible, see N-light | `light/fold5f.done.png` |
| docs updated | `docs/reference/tui.md` yes; DEVIATIONS/design note no (F-dev) | |

## Real-model session (live-tui3)

`hc run --bg --name live-tui3 ... bash.exe -c 'set -a; . <env file>; set +a; ... tui2_scn.py live3 <spec>'`, the
scenario driver running the release binary in a ConPTY inside the pane; env file never printed.

1. "Run these three as three separate bash calls: git --version, echo hello, uname -s ..." gave `✓ 3 calls` then one prose
   sentence; no step/cost line; facts `kelir idle · deepseek/deepseek-v4.1-flash-1017 · 9.5k tok` at the end.
2. "ls /nonexistent_dir_xyz, echo ok, echo fine" gave `✓ 2 calls`, then the red `$ ls /nonexistent_dir_xyz  ✗ exit 2 · 68 B`
   with its tail (`ls: cannot access ... No such file or directory`), then the model's answer naming the failed call.
3. "ls -la, answer with a markdown table" gave one individual `$ ls -la  ok · 96 B` line, prose, a boxed table.

`usage` events in `events.jsonl` for the session: step 1 `cost 0.00018669`, step 2 `cached_tokens 1024`, step 3
`cached_tokens 1280` (the data the facts row no longer shows; N-reach). No leftover kelir/bash/mock from my runs (drivers
kill their mock and kelir by pid; the one `kelir.exe` in `tasklist` belongs to another worktree). Nothing killed by name.

## Process note (not a product finding)

The implementer committed to this worktree while it was under review: `627d882` also swept in my then-uncommitted
`scripts/live/tui3_specs.py` (it is identical to what I wrote and I kept it). My first release binary and the first gate
were at `6d3d8a4`; the screenshots of the window banner and the final gate are at `627d882`/`f57c959`. One writer per
worktree applies to reviewers as well.

## Files I added

`tests/review_tui3.rs` (9 tests, 8 pass, F-cancel `#[ignore]`d), `scripts/live/tui3_specs.py`, this report.
