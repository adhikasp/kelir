# BUILD-INFRA fixes (from docs/reviews/buildinfra-review.md; verdict FAIL, 2 blockers, 10 should-fix)

BUILD-INFRA is merged on main already; your branch is a fresh worktree off main (kelir-wt/buildinfrafix, branch job/buildinfrafix).
Read the review in full (docs/reviews/buildinfra-review.md) and `tests/buildinfra_review.rs` (red tests proving the blockers): make
them pass without weakening them. Until this lands nobody runs `remote-check.sh prune`.

- **B1 prune safety**: delete only directories the script created: a marker file written at creation (with a random token and the
  creator), never follow symlinks, validate the name against a strict pattern, resolve and check the realpath is directly under
  `$HOME`, refuse if the dir contains anything unexpected, `--dry-run` default for the first call (`prune --yes` to delete),
  and keep "last N per job" (`--job`), not the newest N overall (S8). Prove it on the Linux box against a scratch tree with decoys
  (a dir named like a user project, a symlink, a dir with the pattern but no marker).
- **B2/S2/S3/S4 lock**: the lock must never be handed to a second gate: create-and-own atomically (e.g. `mkdir` a unique temp dir,
  write `owner` inside, then `mv`/`ln` into place, so an ownerless lock can never be observed), takeover only after verifying the
  recorded pid AND start time AND command are gone (pid reuse), re-verify under a second atomic step to close the TOCTOU, tie the
  lock to the gate's whole process tree (a killed wrapper must not free the lock while the gate still runs; use a Job
  Object/process group or have the lock holder be the long-lived parent), support an outside driver's plain `mkdir` lock as
  documented (S2) or stop documenting it, exit 75 on timeout, release only by holder. Test with 5 concurrent gates (exactly one
  runs at a time), kill -9 of the wrapper, pid reuse simulation.
- **S1** make the "release stays fat LTO + cgu 1" test parse the TOML (toml crate in dev-deps) instead of `contains`; **S5** a
  leaky test (nextest `LEAK`) must fail the gate (the no-leaked-processes rule is hard); **S6** far-side logs in a per-run dir
  (mktemp), not fixed `/tmp` names; **S7** AGENTS.md says `bash scripts/gate.sh --linux` and the work-dir naming rule; **S9** no
  hardcoded private host name anywhere in scripts or docs a public repo carries: the Linux machine comes only from `--linux=NAME` /
  `KELIR_LINUX`, no default (error with instructions if unset); scrub docs/BUILD.md; **S10** fix the BUILD.md headline suite number
  to the shipped config and the nextest install instructions; **S11** a failing nextest prints the failed test names in the summary.
Gates: `bash scripts/gate.sh --linux=<machine given by the orchestrator in your prompt>` green; the new tests; run the lock tests 5x.
Commit `buildinfrafix: ...` (explicit paths); notes docs/retros/buildinfrafix-impl-notes.md; final answer "buildinfrafix ready: <commit>".
