# tui2 fixes (from docs/reviews/tui2-review.md; verdict FAIL: 2 blockers, 8 should-fix, 7 nits)

Your worktree is kelir-wt/tui2 (branch job/tui2). FIRST `git merge main` (STEER-4, M5-A + fixes, docs, rules) and resolve.
Read the review in full (docs/reviews/tui2-review.md in the worktree), docs/tasks/STEER-10.md, STEER-3.md, docs/design/TUI-v2.md.
Then fix, in order:
- **B1** scrollback order: tool lines must be committed to the scrollback in the order events happened (close/commit the tool
  group BEFORE the prose that follows it). Add a regression test on a scripted turn (calls then answer; answer then calls;
  interleaved) asserting the committed row order.
- **B2** Backspace on an image chip removes the whole chip and deletes its file (wire `Key::RemoveChip`); test it in the editor
  unit tests AND the PTY e2e. S8: on Windows, where bracketed paste arrives as keystrokes, detect a pasted path burst and turn it
  into the chip at paste time (not only at submit); test it with a keystroke burst.
- **S1** no rows vanish while a later block streams; a long block must not become a 3-row ticker (show the tail properly or
  commit finished blocks as they close); **S2** `kelir view` checks the model's vision flag (`models.rs` `vision_for` has no caller)
  and says so; **S3** validate the image bytes (magic number + non-empty), not only the extension; **S4** tell the model that
  `kelir view <path>` exists: one line in the base prompt (keep <= 1 KB, update the byte snapshot test) and the pointer text;
  **S5** show `paused` and `error` state rows (they are the point of requirement 1); **S6** `/collapse` must not expand
  (separate `/expand` and `/collapse`, idempotent); **S7** list-item continuation paragraphs keep their indent (CommonMark).
- The 7 nits: fix the cheap ones, list the rest in DEVIATIONS.md.
- Update docs/reference/tui.md and DEVIATIONS.md for every behaviour you change. Drive the real binary yourself in a pty and
  LOOK at the rendered rows with a terminal emulator replay (the reviewer's scripts under scripts/live/ show how) before reporting.
Gates: fmt, clippy -D warnings, `cargo test --no-fail-fast` (known load flakes are pre-existing, STEER-9), `bash
scripts/remote-check.sh the maintainer kelir-verify-tui2fix`. Never send console control events to group 0; never kill by image name; keep
commands under 8 KB. Commit `tui2fix: ...` (explicit paths), notes docs/retros/tui2fix-impl-notes.md, final answer "tui2fix
ready: <commit>".
