# STEER-2 fixes (from docs/reviews/steer2-review.md; PASS WITH FIXES, 3 should-fix, 3 nits)

Worktree kelir-wt/steer2 (branch job/steer2, already merged with main). Read the review (docs/reviews/steer2-review.md) and make the
reviewer's red tests in tests/review_steer2.rs pass unweakened:
1. An expired store entry on the FIRST read is re-read once (opencode refreshes its own credential when it runs; optionally trigger a
   harmless opencode command that makes it refresh, only if it cannot hurt: otherwise just re-read after a short delay) and only then
   fails with the message naming the expiry time.
2. OpenRouter key sources: `OPENROUTER_API_KEY` FIRST, then `key_env`, then `key_cmd` (the code made `key_env` replace the default).
3. The migration message for a `[providers.<name>]` table with an unknown provider name must say which table and file, and "rename the
   table to `openrouter` or `opencode-go` (kind = ... is no longer used)". Nits 4-6: fix the spaces eaten by line continuations, make
   `kelir doctor` exit 1 only for the SELECTED provider (report the other as info), bound the helper's stdout (e.g. 1 MB, with a clear
   error when exceeded).
Update docs/reference-worthy facts in DEVIATIONS.md. Gates: `bash scripts/gate.sh --linux=<machine from your prompt>`; commit
`steer2fix: ...` (explicit paths); notes docs/retros/steer2fix-impl-notes.md; final answer "steer2fix ready: <commit>".
