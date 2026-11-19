# STEER-5 fixes, round 2: SIGINT during a retry wait is sometimes lost (Linux)

Found by the orchestrator's Linux gate after steer5fix: `e2e_steer5::an_interrupt_during_a_wait_ends_the_turn_stopped`
fails about 2 runs in 6 when the whole suite runs (`cargo test --test e2e_steer5`, 30 s timeout: the turn never ends), and passes
alone 4/4. This is a real race in product code, not load: the interrupt must take effect immediately (brief STEER-5 / review
B2). Reproduce on the Linux box: in the work dir kelir-verify-steer5, loop `cargo test --test e2e_steer5` 10x (see how the
orchestrator ran it: hc exec --on <machine> -- bash -lc 'cd ~/kelir-verify-steer5/src-tree && for i in ...').

Suspects: the interrupt listener is installed per process at start but the wait's select may subscribe after the signal fires
(a lost wakeup: use a watch/Notify with a stored generation or a signalfd/tokio::signal stream created BEFORE the child
announces readiness); the test sends SIGINT before the listener exists; the SIGINT goes to a different process than the one that
waits; a retry wait entered after the signal was consumed by an earlier wait. Fix the root cause (a level-triggered "interrupt
requested" flag that every wait checks on entry and while waiting, set by the handler), add a stress test (e.g. 200 iterations
of start -> wait -> SIGINT -> expect stopped within 2 s, Unix) that fails before the fix, and keep the Windows split as is.
Also run the whole suite 10x on Linux after the fix; all green.

Worktree: your current directory is kelir-wt/steer5 (branch job/steer5). Gates: fmt, clippy -D warnings, `cargo test
--no-fail-fast` on Windows, `bash scripts/remote-check.sh the maintainer kelir-verify-steer5b` for Linux. Commit `steer5fix2: ...`, notes in
docs/retros/steer5fix2-impl-notes.md, final answer "steer5fix2 ready: <commit>".
