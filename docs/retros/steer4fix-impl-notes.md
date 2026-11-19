# STEER-4 fixes — implementation notes (S1, N1, N2, N3)

Brief: `docs/reviews/steer4-review.md` (PASS WITH FIXES: 1 should-fix, 3 nits).
Every finding is addressed; nothing was left undone and there is no new
deviation, so `docs/DEVIATIONS.md` only gained the two behaviours below (the
pause count and the Herdr word) as recorded rules.

## S1 — after a pause `state.turn_steps` read cap + 1

`src/session.rs`, `drive_turn`: the cap test moved *before* the increment, so it
now reads `turn_steps >= cap` and increments afterwards. The count is therefore
the number of steps that ran (`2` of `2`), not one more (`3` of `2`), which is
the number the brief added `turn_steps` for: a steering human sees a pause
coming from a truthful count. The check is still at the top of the step, before
`before_step`, so it still never splits a call from its result, and a turn that
answers on the cap-th step is still `done` (the check runs before a step is
requested, not after one is answered).

`tests/review_steer4.rs`: `state_after_a_pause_counts_the_steps_that_ran` lost
its `#[ignore]` and its "FINDING S1" doc comment now says "(fixed)". The
assertion is unchanged (`turn_steps == 2` with `max_steps = 2`), so the test was
not weakened. The reviewer's suite is now 9 passing, 0 ignored.

## N1 — the idle TUI status hid the cap

`src/tui.rs` `start()` takes the cap (`Option<u32>`) and seeds
`App::status.limit` before the first paint, so a session with a cap reads
`kelir idle · step 0/3 · 0 tok` instead of `kelir idle · step 0 · 0 tok` with the
limit invisible until the first `TurnStarted`. `src/session.rs` passes
`session.cfg.max_steps`. New e2e in `tests/e2e_tui.rs`
(`the_idle_status_line_already_knows_the_cap`, real binary in a pty) asserts the
idle line shows `step 0/3` and that the banner still names the cap, so the two
surfaces cannot disagree.

## N2 — a paused turn reported Herdr `idle`

A pause is the one state in which kelir waits for a human (they say `continue`),
so it now reports a visible word. First I measured what this Herdr accepts
before choosing one: with `scripts/live/m2_osc_probe.py` in a pane on `hc`
0.1.21, `state=paused` and `state=waiting` left `hc get`'s status `null` (the
record is dropped), while `state=idle` and `state=blocked` both showed up. So
the pause reports **`blocked`**, the word Herdr actually displays, and the
choice is documented in `src/herdr.rs`'s header and in DEVIATIONS with the
measurement. `herdr::State::Blocked` is a new variant (word `blocked`);
`Session::finish_turn` maps `outcome "paused"` to it; the Telegram remap is
untouched (it only turns `done` into `idle`). Tests: the unit test
`a_paused_turn_reports_blocked_because_herdr_drops_paused` replaces the old
"kelir never sends `blocked`" assertion (that assertion is now false by design
and its reason — "kelir never waits on a human" — is what N2 disproved), the
exhaustive word/key tests gained the variant, and `tests/e2e_tui.rs` gained
`the_tui_reports_a_pause_to_herdr_as_blocked`, which drives a cap-2 pause in a
pty and asserts the OSC records are exactly `working` then `blocked`.

Two follow-ups this creates, deliberately not in this diff: SPEC's Herdr bullet
used to say "kelir never reports `blocked`" — that sentence is now replaced
rather than kept in force, and `docs/PROGRESS.md`'s M2 row still carries the old
sentence in its historical text (a past-tense record; the orchestrator owns that
file).

## N3 — personal path and hostname in the retro notes

`docs/retros/steer4-impl-notes.md` and `docs/retros/steer4.md` named the
maintainer's home directory and the Linux machine; both now say `<worktree>` and
`<machine>`. `scripts/scrub-check.sh` still reports hits, but every remaining
one is outside this job's files (`docs/QUEUE.md`, `docs/reviews/STEER-1-review.md`,
`docs/tasks/M5-A.md`), and AGENTS.md itself carries the same kind of path, so a
repo-wide scrub is a separate pass for the orchestrator, not a STEER-4 fix.

## Gates (this worktree, branch `job/steer4`)

- `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean.
- `cargo test` green on Windows: 285 unit, 16 `e2e`, 11 `e2e_m3`, 10 `e2e_m4`,
  6 `e2e_m5`, 6 `e2e_steer4`, **17 `e2e_tui`** (was 15: N1 and N2's pty tests),
  23 `e2e_verbs`, review suites 8/5/2/11/19/7/**9** (`review_steer4`, was 8 + 1
  ignored), `review_steer1` 6 (+2 ignored).
- `bash scripts/remote-check.sh the maintainer kelir-verify-steer4fix` on the committed
  HEAD: `FMT-OK`, clippy clean, 290 unit and every suite green, `review_steer4`
  9/9 (no ignored), exit 0.
- No leaked processes: the pty tests kill their child (and the mock server is
  in-process); the only `kelir.exe` processes on the box afterwards belong to a
  sibling worker's worktree, untouched. No `bg-shim` jobs were left.

## What took the time / what I checked twice

- Choosing N2's word took one probe rather than a guess: sending `blocked` to a
  Herdr that dropped it would have been a silent no-op, which is exactly the
  failure mode M2's B1 was. The probe panes were removed with `hc rm`.
- The idle status (N1) had to be seeded at `start()` and not on `TurnStarted`:
  the status line exists before any turn, and that is the state the nit named.
- Changing the Herdr word touched the TUI's own record enumeration and the dead
  `a_blocked_message_is_never_sent` unit test; both were updated, so nothing
  asserts the old rule.
