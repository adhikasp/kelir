# STEER-7 fixes, round 3 (from docs/reviews/steer7c-review.md; verdict FAIL, 2 blockers, 3 should-fix)

Worktree kelir-wt/steer7fix (branch job/steer7fix at 7f22810+; first `git merge main`). Make the committed red tests in tests/review_steer7c.rs pass
unweakened. Principle (state it in SPEC/DEVIATIONS): **no secret ever leaves the session or the job's own shim**: neither the per-job capability nor the
signing key is returned by any verb. Concretely: (B1) `bg-spawn` for an id already in the registry is refused and never answers with key material; the
capability is delivered once, at spawn, to the shim over a channel a same-user tool child cannot read (inherited pipe/handle, or an owner-only file the
shim deletes immediately; not argv/env); (B2) `bg-claim` returns an acknowledgement only, and no verb reads or echoes a caller-named path; wake messages
quote only the session's own records. Should-fix: a monotonic sequence number bound into the MAC together with the job id (replay of an older record is
rejected; final states are terminal); document the Windows same-user memory-read limit precisely instead of claiming more than the boundary gives (and
zero the key on shim exit); correct SPEC/DEVIATIONS wording. Nits: the two mangled log messages; per-id refusal budgets.
Rerun all attack scripts and tests (tests/review_steer7*.rs, scripts/live/steer7*). Gates: `bash scripts/gate.sh --linux=<machine from your prompt>`. Commit
`steer7fix3: ...` (explicit paths); notes docs/retros/steer7fix3-impl-notes.md; report with hc send t1x "steer7fix3 ready: <commit>".
