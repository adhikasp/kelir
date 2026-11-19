# M4 fixes, round 2 (from docs/reviews/M4-review-round2.md) + repo scrub

Read the round-2 review. `tests/review_m4b.rs` has red tests (B4, B7 at least): make them green
without weakening them; every other suite stays green on Windows and (via
`bash scripts/remote-check.sh the maintainer`) Linux.

- **G1** pid identity: compare the recorded process start time exactly (no 120 s tolerance); if the
  platform cannot give a start time, refuse to signal rather than guess. Record why in DEVIATIONS.md.
- **G2** a `lost` promoted job must be cancellable (reach its tree by job-object name on Windows /
  process group on Unix) or `cancel` must say precisely why not and `ls` must not show it as running.
- **G3** Windows: spawning the bg-run shim with `DETACHED_PROCESS` stalls console allocation under
  load (40 concurrent bash starts: median 8 s vs 0.1 s with `CREATE_NO_WINDOW`). Use
  `CREATE_NO_WINDOW` (+ new process group) while keeping the detach semantics the tests prove
  (job survives the session, not in the call's Job Object). Re-measure with a test or script and put
  the numbers in DEVIATIONS.md.
- **G4** the Unix linger loop (25 ms /proc polling, ~23% of a core): replace it with a blocking /
  event-driven wait or at least an adaptive backoff (e.g. 25 ms growing to 1 s) and prove the CPU use is
  small (measure with a test or a script; put the number in DEVIATIONS.md). Keep the "finished call's
  leftover tree dies with a killed session" guarantee.
- **Repo scrub (public repo rule)**: no personal names, hostnames or private paths in any tracked
  file: replace `the maintainera`, `a Linux box`, `the maintainer`, `a Linux box`, `a build box`, `a device` and the like in
  `docs/SPEC.md`, `docs/DEVIATIONS.md`, `docs/PROGRESS.md`, `docs/reviews/*`, `docs/tasks/*`,
  `Cargo.toml` (authors) with neutral wording (`the maintainer`, `host A`, `a Linux box`). The
  scripts keep taking the machine name as an argument (the orchestrator passes it); AGENTS.md
  and the task docs must not name it. Add `scripts/scrub-check.sh <pattern-file>` that fails if any
  tracked file matches patterns read from a file kept OUTSIDE the repo (the file path is an
  argument), so the check itself does not leak the names; run it with the orchestrator-provided file
  `C:/Users/the maintainer/kelir-scratch/scrub-patterns.txt` before reporting. (Git history keeps old names;
  that is handled at publish time, do not rewrite it.)

Gates: fmt, clippy -D warnings, full `cargo test` (run it twice), `bash scripts/remote-check.sh
the maintainer`, scrub-check clean. Update the `docs/PROGRESS.md` M4 row. Commit `M4: fixes 2` (explicit
paths), `hc send t1x "<result>"`.
