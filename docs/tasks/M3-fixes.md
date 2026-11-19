# M3 fixes (from docs/reviews/M3-review.md)

Read the whole review first. `tests/review_m3.rs` has red tests that prove the findings: make them
all green without weakening them; every other suite stays green on Windows and (via
`scripts/remote-check.sh`) Linux.

Fold semantics you must end up with (decide the details, document them in DEVIATIONS.md):
- The request after a compaction is `[system, <summary>, <pins in original order>, <kept tail>, ...later events]`
  in one defined order: the summary and pins come first, the kept tail after, and the **last message
  is always the newest actual event** (never an old pin). It always starts with a `user`-role message
  after the system message (provider-safe), and tool-call/tool-result pairs are never split by the tail.
- A later compaction supersedes an earlier one: exactly one summary and each pinned message exactly
  once; the second summarising request includes the first summary as input.
- Cost control: skip a compaction that cannot shrink the context (N1) and say so in the event/result.

Fix all findings (B1, B2, the four should-fix, N1-N5). N2: re-queue an agent-called compact on resume
if it was settled but not applied. N3: make "already running" atomic on Unix (a lock file with
exclusive create/`flock`, or equivalent) and prove it with a repeated race test. N4: log the
summarising request's usage/cost as a `usage` event with a marker so `kelir ls` sees it.

Gates: fmt, clippy -D warnings, full `cargo test`, `bash scripts/remote-check.sh <machine>`. Update the
`docs/PROGRESS.md` M3 row. Commit `M3: fixes` (explicit paths), `hc send t1x "<result>"`.
