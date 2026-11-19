# STEER-6 fixes (from docs/reviews/steer6-review.md; verdict FAIL, 1 blocker)

Read the review in full (it is in your worktree: docs/reviews/steer6-review.md) and docs/tasks/STEER-6.md.

0. FIRST `git merge main` into your branch (main has STEER-4, M5-A, M5-A fixes and docs) and resolve conflicts; compile and
   pass before anything else; commit the merge.
1. **B1**: with `shell = ["cmd.exe","/C"]` (and the no-bash fallback) the long route drops `/C` and nothing runs. Fix, with
   the reviewer's red test passing; also decide and test PowerShell (`-File cmd.ps1`) or exclude it (N2).
2. **S1**: use ONE route. On Windows **always** hand the command to the shell as a script file (the retro suggested "always"),
   so semantics never change at a length threshold: backslashes, quotes, `$`, unicode, CRLF terminators survive byte-exactly
   at every length (property-style test across lengths 1..9000 with backslash runs). Keep exit codes, cwd, env. On Unix keep
   `-c` unless you can show a reason; the same test passes there.
3. **S2**: `parallel_tool_calls` must not silently depend on a slow/missing model list: decide the rule (e.g. a conservative
   built-in allow-list of families known to support it + the list when available, cache refresh without waiting for a `model`
   verb) and test the first-session/slow-fetch case; **S3**: gate `reasoning_effort` on advertised `supported_parameters`
   where the provider lists them, send it for unknown models only when explicitly configured, and on an HTTP 400 that names the
   field retry once without it and remember for the session (log an `error` note); default stays `medium` only where
   supported.
4. Nits: N1 truncation notice only when exit code != 0 or the marker is in the last lines; N3 add the byte-exact snapshot of
   the rendered base prompt; N4 document the CRLF normalisation (and keep a way to send raw CR if cheap).
5. Update docs/reference-worthy facts into DEVIATIONS.md (the reference docs job will pick them up).

Gates: fmt, clippy -D warnings, `cargo test --no-fail-fast` (3 runs; load flakes review_m4 r18/r19, e2e_verbs are pre-existing,
STEER-9), `bash scripts/remote-check.sh the maintainer kelir-verify-steer6fix`. Commit `steer6fix: ...` with explicit paths; notes to
docs/retros/steer6fix-impl-notes.md; final answer "steer6fix ready: <commit>". Remember: commands over 8 KB are silently cut on
this shell until your own fix lands: write big files with the edit tool or small commands.
