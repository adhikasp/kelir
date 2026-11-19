# STEER-4 review (no step limit by default; an explicit cap pauses the turn)

Reviewer: fresh QA agent. Diff reviewed: `9826dbe..a87b5ff` (8 commits, `job/steer4`).
Brief: `docs/tasks/STEER-4.md`. Spec: `docs/SPEC.md`, `docs/DEVIATIONS.md`.

## Verdict: PASS WITH FIXES (0 blockers, 1 should-fix, 3 nits)

Every acceptance line of the brief holds, on Windows (unit, mock e2e, and a real model in Herdr
panes) and on Linux. One honest-reporting defect remains (S1: after a pause the state reports one
step more than ran). It does not change behaviour or the log, so it does not block the merge, but
it should be fixed before the TUI/`ctl` surface is relied on to "see a pause coming".

## Gates (run independently)

| Gate | Result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --all-targets -- -D warnings` | clean (also with my new test file) |
| `cargo test` (Windows, debug) | 285 unit; e2e 16, e2e_m3 11, e2e_m4 10, e2e_m5 6, **e2e_steer4 6**, e2e_tui 15, e2e_verbs 23; review_m0 8, m1 5, m2 2, m3 11, m4 19, m4b 7, steer1 6 (+2 ignored); 0 failed |
| `scripts/remote-check.sh <machine> kelir-verify-steer4-rev` (HEAD `a87b5ff`) | FMT-OK, CLIPPY-DONE, 290 unit + all e2e/review suites green, EXIT=0 |
| New: `cargo test --test review_steer4` | 8 pass, 1 ignored (FINDING S1) |
| Release build | `target/release/kelir.exe` 6.4 MB (budget 20 MB) |
| Leaked processes | none: no `kelir.exe` left after the live panes; panes ended or were killed by terminal id |

## Acceptance lines of the brief

| Line | Evidence | Verdict |
|---|---|---|
| unset / `0` = unlimited (`Option<u32>`), explicit positive = cap per turn | `config.rs:262` `step_cap`; unit `max_steps_is_unset_by_default_and_zero_means_unlimited`; mine `the_stopgap_and_zero_in_the_file_run_a_turn_to_its_end` | ok |
| invalid value (`-1`, `"five"`, `1.5`) is a config error naming the key | unit + mine `a_bad_max_steps_in_the_file_is_a_usage_error` (no request reaches the provider) | ok |
| `--max-steps N` overrides; flag on `run`/`ask`/`tui`/`resume`, falls through before the subcommand | unit `the_max_steps_flag_is_common...`; e2e `the_flag_overrides_the_config_and_zero_lifts_a_cap`; mine `bad_values_on_the_flag_are_refused` (`-1`, `abc`, `4294967296`, `1.5`, empty are all refused, exit 2), `ask_accepts_the_flag`, `resume --max-steps` in the chain test | ok |
| cap hit = paused, not failed: `turn_ended outcome=paused`, message "turn paused after N steps; say continue to resume", `result.outcome=paused`, exit 0 | e2e `an_explicit_cap_pauses_the_turn_and_exits_zero`; live (below) | ok |
| check is at a step boundary, never between a call and its result | `session.rs:2182` is before `before_step`; mine `a_pause_keeps_the_last_words_and_never_splits_a_call_from_its_result` (all `tool_started` have a `tool_settled` before `turn_ended`) | ok |
| a turn that *answers* on the cap-th step is `done`, not `paused` | mine `a_turn_that_answers_on_the_capth_step_is_done_not_paused` (cap 3 with the answer as step 3; cap 1 with an instant answer) | ok |
| `--keep` stays alive; next `say` starts a new turn with a fresh count | e2e `after_a_pause_the_next_say_starts_a_fresh_count`; live cap scenario | ok |
| resume treats a paused turn like a stopped one | e2e `resume_continues_a_paused_turn`; unit `interrupted(paused)`; mine pause/resume/pause/resume chain: steps 1..6 unique, 5 tools run once, every later request an append-only extension of the previous one (cache prefix holds), resumed request ends on a `tool` message; live | ok |
| TUI banner / status shows steps used/limit only when a limit is set | unit `the_status_line_shows_steps_used_and_limit_only_when_a_cap_is_set`; live TUI panes (below) | ok |
| docs: SPEC, DEVIATIONS (README/usage) | SPEC `turn_ended` vocabulary, loop paragraph, `resume` row; DEVIATIONS entry and the resume note; `--help` text. The repo has no README, so "README/usage" is the CLI help only | ok |
| stopgap `max_steps = 4294967295` still parses | unit; mine e2e; live `stopgap` | ok |

## Findings

| # | Severity | Where | Finding |
|---|---|---|---|
| S1 | should-fix | `src/session.rs:2181-2186`, `:1277` | `turn_steps` is incremented *before* the cap test and not rolled back when the turn pauses, so after a pause a verb's `state` reports `turn_steps = cap + 1` (e.g. `turn_steps 6, max_steps 5`, seen live; `3` of `2` in the mock). Only `cap` steps ran. The TUI is not affected (`UiEvent::Paused` sets the count to the cap), but `state` is exactly the surface the brief added so a steering human "sees a pause coming". Failing test: `cargo test --test review_steer4 -- --ignored state_after_a_pause_counts_the_steps_that_ran`. Fix: test `turn_steps >= cap` before incrementing, or `self.turn_steps -= 1` on the pause path. |
| N1 | nit | `src/tui.rs` (status at idle) | With a cap set the idle status line reads `step 0`, not `step 0/3`: the limit only reaches the status on `TurnStarted`. The banner names the cap, so nothing is hidden. |
| N2 | nit | `src/session.rs:1222-1231` | `finish_turn("paused")` reports Herdr state `Idle`. A pane supervisor cannot tell a paused turn from a never-started one; arguably it should be a visible state. Not specified by the brief. |
| N3 | nit | `docs/retros/steer4-impl-notes.md` | The retro notes contain a username path and a machine hostname. AGENTS.md says the repo is public with no personal paths or hostnames. `AGENTS.md` itself already carries such a path, so there is precedent, but the notes should be scrubbed or kept outside the repo. |

Design observation (not a finding): for a non-`--json` `kelir run`, `paused` exits 0, so a shell
script distinguishes done from paused only through stderr or `--json`'s `result.outcome`. That is
the documented behaviour (DEVIATIONS, brief) and the hint is deliberately printed even under
`--json`.

## Live drive (real model, Herdr panes, `deepseek/deepseek-v4.1-flash-1017`)

Script: `scripts/live/steer4_live.sh <scenario> <scratch>`; each run was a pane started with
`hc run --bg --name live-steer4-<scenario> ... bash -c 'set -a; . <env file>; set +a; ...'`. The
key was never printed or copied. State, config and cwd are isolated (own `KELIR_CONFIG`,
`KELIR_STATE_DIR`), so the user's real config played no part.

1. **No limit, > 100 steps** (`nolimit`, no `max_steps` anywhere, 110 separate `echo N >> counter.txt` calls):
   `exit=0 steps=112 outcomes=done`; `counter.txt` has 110 lines;
   `{"type":"result","outcome":"done","exit_code":0,"steps":112}`. Verdict: PASS (the old default stopped at 100).
2. **Cap of 5 then `say continue`** (`cap`: `run --keep --json --max-steps 5`, then `kelir ctl latest say continue`):
   after turn 1 `steps=5 outcomes=paused`; the ctl answer showed `"turn_steps":6,"max_steps":5` (S1, live);
   after the second turn `steps=10 outcomes=paused paused`, `counter.txt` 10 lines, i.e. steps 6-10 ran
   with a fresh count; `ctl end` returned ok and the process exited 0; stderr carried
   `→ turn paused after 5 steps; say continue to resume` once per pause. Verdict: PASS (+S1).
3. **Resume of a paused turn** (`resume`: config `max_steps = 4`, 10-step task):
   run 1 `exit=0 steps=4 paused` (`result.outcome=paused, exit_code 0`); `kelir resume <id>` `steps=8 paused`
   (a fresh 4); `kelir resume --max-steps 0 <id>` `steps=12 done`; `counter.txt` = `1 2 ... 10`, no
   line repeated, so no settled call re-ran. Verdict: PASS.
4. **Stopgap** (`stopgap`: config `max_steps = 4294967295`): a 2-step turn `exit=0 steps=2 done`. Verdict: PASS.
5. **TUI** (`kelir tui --max-steps 3` and `kelir tui` in two panes; screens read with `hc read`):
   - banner with a cap: `step cap 3 per turn · reaching it pauses the turn (say continue)`; none without one;
   - cap pane after the task: scrollback `turn paused after 3 steps; say continue to resume`, status
     `kelir paused · deepseek/deepseek-v4.1-flash-1017 · step 3/3 · 3.3k tok · 50% cache · $0.0003`;
   - after typing `continue`: steps 4-6 ran, paused again with `step 3/3` (fresh count);
   - no-cap pane: `kelir done · ... · step 10 · 12.8k tok ...` (no `/limit`).
   Verdict: PASS. No pixel screenshot: the primary monitor showed a browser, not the pane, so I
   deleted that capture and rely on `hc read`, which prints the pane's rendered screen.

Spend: about 150 cheap-model steps in total; the largest run was the 112-step turn.

## Test judgment

The implementer's six e2e tests cover the brief's list (150 steps no cap, cap 5 pauses, `--keep`
continue with a fresh count, resume of a paused log, flag override, state fields) and assert on the
log, NDJSON, exit code and mock request counts rather than mirroring the implementation. What was
missing, and what `tests/review_steer4.rs` now adds: the cap boundary (finishing on the cap-th step
is `done`), `final_text` after a step that talked, the stopgap value and `0` in the file, bad
values on the flag and in the file, repeated pause/resume with cache-prefix and step-uniqueness
checks, the `ask` flag, and `state` after a pause (S1, ignored until fixed).

## Spec items not covered / out of scope

- Brief item "the orchestrator deletes the `max_steps` line from the user config" is outside this
  worktree and was not touched; the stopgap value still parses (live scenario 4).
- `docs/PROGRESS.md` row is the orchestrator's after merge.
- The implementer noted one flaky pre-existing test (`e2e_verbs::say_mid_turn_is_in_the_next_request`,
  a `sleep 1` race under load). It passed in my full run; unrelated to this change.
- `KELIR_GO_API_KEY` is not available: the Go provider path was not live-verified (the change is
  provider-independent; mock-only).
