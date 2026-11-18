# Review and test protocol (for the reviewer/tester agent)

You review milestone **Mn** of kelir. You do not write product code. You may add or fix tests
under `tests/`, scripts under `scripts/live/`, and write `docs/reviews/Mn-review.md`. Commit only
those, as `Mn-review: …`. Start no other agents (Herdr panes for driving the binary are fine).

## 1. Review (read the diff, not the summary)

`git diff <previous-milestone-commit>..HEAD`, against `docs/SPEC.md` and `docs/tasks/Mn.md`.
Look for: spec requirements missing or only half done; behaviour that contradicts the spec;
Windows-only or Unix-only assumptions without `cfg`; cache-prefix violations (anything that edits
earlier messages); data loss paths (log not fsync'd, torn lines, unsettled calls re-run);
tests that assert nothing or only mirror the implementation; secrets or personal data in the repo;
dependencies outside the spec; footprint (release binary size, idle RSS). Every finding gets a
severity (blocker / should-fix / nit), a file:line, and a concrete failing scenario.

## 2. Pyramid, independently

Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` yourself and
paste the counts. Then judge whether the unit and e2e tests actually cover the milestone's
"how you know it works" column in the spec; add the missing tests that you can (a failing test
that proves a real bug is the best finding).

## 3. Live drive (the top of the pyramid)

Build `cargo build --release`. Drive the **real binary against a real model in a Herdr pane**, not
via the mock: `hc run --bg --name live-Mn --cwd <scratch dir> bash -lc '<command>'`, watch with
`hc read live-Mn`, wait with `hc wait live-Mn`, never `sleep`-poll. Use a scratch directory
outside the repo (`C:/Users/the maintainer/kelir-scratch/Mn`). Provider: OpenRouter with a cheap model
(`deepseek/deepseek-v4.1-flash-1017`); `OPENROUTER_API_KEY` is in your environment (never print
it, never write it to a file). `KELIR_GO_API_KEY` is not available: mark the Go path
"not live-verified" and verify it against the mock only.

The milestone's live scenarios are in `docs/tasks/Mn.md` under "Live drive". Beyond those, try to
break it: kill mid-tool, odd input, huge output, Ctrl-C, closing stdin. Record for each scenario
the exact command, what you saw (quote the relevant `events.jsonl` lines, `jq` them), and a
verdict. Keep total spend small (a handful of short runs).

## 4. Report

Write `docs/reviews/Mn-review.md`: verdict (PASS / PASS WITH FIXES / FAIL), test counts, findings
table, live-drive log, spec items not yet covered. Then
`hc send t1x "Mn review: <verdict>; <n> blockers, <n> should-fix; see docs/reviews/Mn-review.md"`.
