# M4 fixes (from docs/reviews/M4-review.md)

Read the whole review first. `tests/review_m4.rs` has red tests proving the findings (Windows 12/19,
Linux 14/19): make them all green without weakening them; every other suite stays green on Windows
and (via `scripts/remote-check.sh`) Linux. Fix B1, B2, the four should-fix and the nits that are cheap.

Constraints to hold together (they pull against each other; the design must satisfy both):
- an **unpromoted** call's whole tree dies with the session (incl. exec'd grandchildren such as
  `sleep.exe` and nested `bash` under msys on Windows), and `bg cancel` kills the whole tree;
- a **promoted** job and an explicit `kelir bg run` job survive the session on purpose;
- `cmd &` inside a finished call: decide per the spec ("detached bg is `kelir bg`; a plain `&` is
  not tracked") and make it deterministic and documented, not an accident of the Job Object flags.
- `--cwd` and argv quoting for `bash` calls and `bg run` are preserved exactly (R1, R2).
- a dead shim must not leave state `running` forever, and cancel must not kill a recycled pid
  (verify the pid's identity: start time or a token held by the shim).

Gates: fmt, clippy -D warnings, full `cargo test`, `bash scripts/remote-check.sh <machine>`. Update the
`docs/PROGRESS.md` M4 row. Commit `M4: fixes` (explicit paths), `hc send t1x "<result>"`.
