# STEER-7 fixes, round 4 (from docs/reviews/steer7d-review.md; FAIL: 1 blocker, 3 should-fix, 3 nits)

Worktree kelir-wt/steer7fix (branch job/steer7fix at 1172a08+, first `git merge main`). Round-3 findings are verified closed; make the committed tests in
tests/review_steer7d.rs pass unweakened. Fix:
- **Blocker**: a refused (or any) `bg-spawn` must never write the caller's `env` or `token` into `events.jsonl`: log a redacted record (verb, dir, cwd, a count of
  env vars) only; and audit EVERY `log_verb`/`refuse_verb` path so no verb logs request args wholesale. Add a generic test: for each verb, a request containing
  a canary secret in any arg never appears in events.jsonl, NDJSON stdout, `observe`, or error text. Also scrub provider key env vars from the environment of tool
  children where it is cheap and safe (the child must still be able to run `kelir`/git/cargo; decide and document).
- **S1**: the session sets `kind=job`, its own `sock`, its own `token`, the `job` id and validates `call` against `live_calls`; accept from the request only
  `cmd`, `shell`, `cwd`, `env` (env filtered). Every hardening (`harden_process` etc.) applies to every shim kind.
- **S2**: apply the same-user memory-access denial (Windows SDDL/`deny_same_user_memory_access`, Linux `PR_SET_DUMPABLE=0`) to the session process too (keep the
  right for the harness to kill it), or move the keys so only the shim holds them; whichever you choose, make SPEC/DEVIATIONS exactly honest about what protects
  what on each OS. **S3** follows from S1/S2.
- **Nits**: clean UTF-16 NUL in the SDDL literal; remove dead `bg-claim` (or give it its own budget); validate `call` in `bg-spawn`; use
  `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` for the capability pipe on Windows if it is straightforward.
Rerun all attack tests/scripts (tests/review_steer7*.rs, scripts/live/steer7*). Gates: `bash scripts/gate.sh --linux=<machine from your prompt>`. Commit `steer7fix4:
...` (explicit paths); notes docs/retros/steer7fix4-impl-notes.md; report with `hc send t1x "steer7fix4 ready: <commit>"`.
