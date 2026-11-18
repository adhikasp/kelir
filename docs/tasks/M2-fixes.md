# M2 fixes (from docs/reviews/M2-review.md)

Read the review and `scripts/live/m2_osc_probe.py` first. Make `tests/review_m2.rs` fully green
without weakening it; all other suites stay green on Windows and (via `scripts/remote-check.sh`) Linux.

- **B1 (blocker)** Herdr status. This Herdr (`hc 0.1.21`) only takes a record that is exactly
  `state=<word>` (see the probe). Emit what it accepts, keeping kelir's identity/label information
  in whatever form the probe shows Herdr tolerates (if nothing else is tolerated, drop it). `hc get <pane>`
  must show `working` during a turn and `done`/`idle`/`error` after. **Verify it yourself in a real Herdr
  pane**: `hc run --bg --name osc-check --cwd <scratch> "C:/Program Files/Git/bin/bash.exe" -c '<run
  kelir TUI against scripts/live/canned_server.py with KELIR_HERDR=1>'`, drive with `hc send`/`hc keys`, and
  poll `hc get osc-check` for the status transitions. Add that as a script under `scripts/live/` and
  assert the exact bytes of every record in a unit/PTY test (N3). Update DEVIATIONS.md: the old claim
  about the format was wrong; record what Herdr accepts and how it was verified.
- **F2 (should-fix)** one stdout writer. Route status records through the TUI task (or one shared
  lock around every stdout write incl. crossterm) so an OSC never lands inside a CSI. Non-TUI
  paths (`run`) keep their behaviour.
- **F3 (should-fix)** after `/quit` or a double Ctrl-C the shell prompt/output must start on a new
  line **below** the viewport, never overwriting the banner. Investigate in a real pane (`hc read`
  after `echo after: $?`). Likely: leave the cursor at the bottom of the inline area and print a
  newline, show the cursor, and do not clear the viewport rows above the banner.
- **N1** idle Ctrl-C reply must say the stop was dropped, not "now". **N2** flatten tui flags so bare
  `kelir --model x` works (or document precisely). **N3** see B1. **N4** make wrapping incremental or
  bounded (cache the wrapped prefix) with a test on a 1 MB message. **N5** add a test-only panic
  injection (env `KELIR_TEST_PANIC=1`, documented) and test that the terminal is restored.

Gates: fmt, clippy -D warnings, full `cargo test`, `bash scripts/remote-check.sh the maintainer`. Update the
`docs/PROGRESS.md` M2 row. Commit `M2: fixes` (explicit paths), `hc send t1x "<result>"`.
