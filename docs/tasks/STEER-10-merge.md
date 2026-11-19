# tui2: bring the branch up to date with main (merge + resolve)

tui2fix is done on branch job/tui2 (docs/reviews/tui2-review.md findings fixed). Main moved: STEER-5 (retry/backoff, paused outcome,
interrupts, governor), STEER-6 (prompt lines, shell route, reasoning_effort gate, multi-edit, compact_at_tokens), STEER-4, M5-A fixes.
A dry-run `git merge --no-commit --no-ff job/tui2` into main conflicts in: docs/PROGRESS.md, src/models.rs, src/prompt.md,
src/session.rs, src/tui.rs.

Your worktree is kelir-wt/tui2 (branch job/tui2). Do:
1. `git merge main` into the branch and resolve every conflict keeping BOTH sides' behaviour and tests: the TUI v2 state machine,
   rows, markdown, tool collapse, image chips and `kelir view` AND STEER-5's retry state/countdown (the `rate limited` state row must
   be fed by the new retry events), the single `paused` outcome (state row `paused`), STEER-6's prompt lines. `src/prompt.md` must
   keep <= 1 KB: both sides added lines; merge them concisely, update the byte snapshot (tests/fixtures/prompt.md.snapshot) and the
   size test deliberately; mention the `kelir view` line (S4) and the STEER-6 lines. Keep every test of both sides.
2. Make sure the activity indicator shows the retry countdown from STEER-5's events (rate limited, retrying in N s, attempt n) and
   pause/error rows work with the unified paused outcome; add/adjust tests.
3. Gates: fmt, clippy -D warnings, `cargo test --no-fail-fast` on Windows (3 runs, STEER-9 flakes pre-exist), Linux via `bash
   scripts/remote-check.sh the maintainer kelir-verify-tui2m`. Drive the real binary in a pty and LOOK at the replayed rows (scripts/live shows
   how) for idle/thinking/streaming/tool/rate-limited/paused/error. Commit `tui2m: ...` (explicit paths), notes
   docs/retros/tui2m-impl-notes.md, final answer "tui2m ready: <commit>".
