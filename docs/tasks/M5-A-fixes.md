# M5-A fixes (from docs/reviews/m5a-review.md)

Read the review in full. `tests/review_m5a.rs` has ignored FINDING repro tests (10): un-ignore them and make them
all pass without weakening them; every other suite stays green on Windows and Linux. Fix F1, F1b, F2, F2b, F3,
F4, F5, F6, F7, F8, F9 and the cheap nits (N1, N2, N3, N5; N4: report a deleted source once, or document why not).

Design guidance (decide the details, record them in docs/DEVIATIONS.md):
- The watcher tracks the **current** hash per source (not "every hash ever carried"), so A->B->A is announced as A
  again; the `skills` verb and the watcher share one code path and one hashing rule (whole file vs body: pick one),
  so a skill is never announced twice and its `description` always arrives.
- No sentinel paths: `embedded` is a typed variant (`SourcePath::Embedded | File(PathBuf)`), a config
  `prompt = "file"` is recorded with its real path.
- Instruction discovery keeps **every** AGENTS.md/CLAUDE.md found walking up (nearest last or first, per the brief),
  stops at the repo root or home (N3), reports non-UTF-8 (lossy decode + a `source` note) and malformed skills
  (skipped with a `source` note listing why), and caps sizes (per source and total; truncation is stated in the text).
- `kind_of` comes from where the source was found, not from its name.
- Resume does not re-announce an edit its own log already carries (rebuild the watcher from the folded state).
- Env scrub in tests covers every `KELIR_*` and `TERM_PROGRAM`.

Gates: fmt, clippy -D warnings, full cargo test, `bash scripts/remote-check.sh the maintainer kelir-verify-m5afix`,
no leaked processes (AGENTS.md rules). Commit `M5-A-fixes: ...` (explicit paths).
