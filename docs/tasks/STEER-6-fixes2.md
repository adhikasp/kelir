# STEER-6 fixes, round 2 (from docs/reviews/steer6b-review.md; PASS WITH FIXES)

In your worktree: `git merge main` first, then fix (read docs/reviews/steer6b-review.md in the worktree):
- **N6**: the cmd.exe `.bat` route has no `@echo off`, so every result carries a prompt echo. Emit `@echo off` (and keep
  exit codes/cwd/env) with a test on the cmd.exe route.
- **N5**: a shell argv whose last word is not `-c`/`/C`/`-Command` (e.g. `["bash","-lc"]`) silently keeps the old
  backslash-halving route. Recognise the combined short flags (`-lc`, `-ec`, `-xc`, ...) so the file route applies, and add a
  test; if any other shape cannot be handled, make it an error with a clear message, not silent halving.
- Cheap remaining nits from that review (N-numbers listed there) if they are one-liners.
Gates: fmt, clippy -D warnings, `cargo test --no-fail-fast` (known load flakes are pre-existing, STEER-9), remote-check with the
work dir name kelir-verify-steer6c. Commit `steer6c: ...`, notes in docs/retros/steer6c-impl-notes.md, final answer
"steer6c ready: <commit>".
