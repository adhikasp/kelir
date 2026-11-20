# STEER-12 fixes (from docs/reviews/steer12-review.md; PASS WITH FIXES, 0 blockers, 4 should-fix)

Worktree kelir-wt/steer12 (branch job/steer12, merged with main at b16f634). Read the review in full first. Fix:
- **S1 batching vs the 10 KB spill**: the "one bash call carries every read" rule collides with the spill limit (a 42 KB labelled batch came back as
  2.5 KB with three of four sections gone and a tail that looks complete). Resolve it: either make the prompt say batches must stay small (~8 KB of
  output; split bigger ones) or make the spill notice for a batched/labelled output keep each `=== label` header visible and say which sections are
  missing; prefer the notice change if it stays deterministic and small, and test it with a 40 KB batch (what the model sees, which sections).
- **S2** the metrics classifier counts writes as reads (redirects, `sed -i`, `find -delete`, `git branch -D`, `git config k v`, `sleep`, `cat > f <<EOF`,
  `git show X:f > /tmp/f`): fix, add the reviewer's failing tests, recompute the numbers. **N3** quoted powershell/`bash -c 'cat f'` payloads and `\"`
  inside quotes.
- **S3** the BEFORE sessions did not run the prompt being replaced (only one of six ran the "batch independent reads" prompt; three ran a 277-char
  pre-M5 prompt): rebuild the baseline so it is honest: split BEFORE by the prompt each session actually ran (read `session_started.system.text`), and
  label it; do not claim "old prompt" for all six. **N4** state the real threshold (>= 1000 completion tokens reproduces the brief's 271/180 counts,
  not 2000) and the commit counting.
- **S4** false premise: `parallel_tool_calls` is sent only for OpenRouter (src/provider.rs ~209), never on opencode-go; remove the parallel-calls claim
  from the prompt and docs/PROMPT.md/retro (keep the one-command batching rule, which always works), or enable the flag for opencode-go only if the
  gateway demonstrably honours it (test through the logging proxy); the prompt must stay true. **N1** drop the obsolete/ambiguous `<8 KB` wording.
  **N2** say WHERE notes go: outside the repo or in a git-ignored scratch path (add it to .gitignore) so notes never pollute commits.
Re-run the reviewer's small A/B (old vs new prompt on a read-heavy task on the OpenRouter cheap model; use `OPENROUTER_API_KEY` from your env) after
the prompt change and record the result honestly in docs/PROMPT.md. Gates: `bash scripts/gate.sh --linux=<machine from your prompt>`. Commit
`steer12fix: ...` (explicit paths); notes docs/retros/steer12fix-impl-notes.md; final answer "steer12fix ready: <commit>".
