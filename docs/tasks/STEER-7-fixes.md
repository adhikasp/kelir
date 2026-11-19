# STEER-7 fixes (from docs/reviews/steer7-review.md; verdict FAIL, 1 blocker)

Worktree kelir-wt/steer7 (branch job/steer7, merged with main). Read the review in full and make `tests/review_steer7.rs` (the two red
tests) pass unweakened.

**B1 (headline)**: a tool child of the session holds the session token by design, yet it must not be able to impersonate the bg shim
(inject `bg_started`/`bg_settled`/"bg job was lost" events) or cancel jobs it did not start. File existence (`commands/<id>/` or
`bg/<id>.status`) is NOT evidence: the child can create those. Design to implement:
- The **session is the only issuer of job ids**: ids are minted by the session (random 128-bit component, not deterministic call ids) when
  it handles `bg run` / `bg-spawn` / a promoted call, recorded in an in-memory registry AND in the session's own event log
  (`bg_started`), and rebuilt from the log on resume.
- Each job gets its own **capability** (random per-job secret) handed only to that job's shim (argv/env of the shim process or an
  inherited handle, never logged); `bg` reports for a job id require that job's capability (constant-time compare) in addition to the
  session token; `cancel` by a token holder is allowed only for ids in the registry.
- A report or cancel for an id not in the registry is refused with a `verb` event marked refused (rate-limited so a child cannot flood
  the log).
- **S2**: no deterministic ids across sessions: call-directory names may stay, job/registry ids are unique and namespaced.
**S1**: a sibling process of the same user can read another session's token file: that is inherent (same-user trust boundary). Say so
precisely in SPEC/DEVIATIONS (threat model: protects against other users and against accidents/impersonation by unprivileged code that
lacks the token or the job capability; NOT against same-user code that can read the file) and correct the brief's test line; consider
tightening where cheap (token file readable only while the session lives; removed on exit).
**Nits**: N1 reject Windows reserved device names and trailing dot/space in job ids; N2 `bg/` dir mode 0700 explicitly; N3 cap the
request line length (e.g. 64 KB) and set a read timeout for unauthenticated peers.
Attack scripts from the reviewer under scripts/live/ must all be refused after your fix (rerun them). Gates: `bash scripts/gate.sh
--linux=<machine from your prompt>`; commit `steer7fix: ...` (explicit paths); notes docs/retros/steer7fix-impl-notes.md; final
answer "steer7fix ready: <commit>".
