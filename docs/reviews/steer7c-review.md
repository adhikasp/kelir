# STEER-7 review, round 3 (adversarial) — verdict: FAIL (2 blockers, 3 should-fix, 2 nits)

Reviewer: fresh Claude Code session. Branch `job/steer7fix` at e4107e5 (round-3 fix + merge of main). Windows 11 and Linux. Gates: Windows 937/942 passed,
Linux 939/943; every failure is one of the new tests in `tests/review_steer7c.rs` (committed in 7f22810). Live drive: real release binary, a real bg job in a
Herdr pane with the cheap model. The detailed reproduction lives only in those tests (the full write-up was deliberately kept out of this file).

## Blockers
- **B1** `src/session.rs:2104`, `src/bg.rs:794-833`, `src/trust.rs:872`: `bg-spawn` naming a job id the session already owns answers *any token holder* with that
  job's capability and signing key. Every tool child holds the session token, so a child obtains the key of a running job and can sign records the session accepts
  (a `bg_settled` plus a wake message from `job`). This is the round-2 outcome again by a different path. Fix direction: a job capability or signing key is
  **never returned to a caller**; it is handed once to the shim at spawn over a channel the child cannot read; `bg-spawn` for an id already in the registry is refused.
- **B2** `src/session.rs:2131`: `bg-claim` also returns the private key to the caller, and the session puts the tail of an arbitrary file named in the request
  into the wake message. Fix direction: `bg-claim` returns no secret (acknowledgement only); never read or echo a caller-named path; only the session's own
  `bg/` records are read.

## Should-fix
- **Replay**: the signed record has no counter or nonce; an older signed `running` record placed over a final `exited` one makes a resumed session report the job
  lost. Fix direction: a monotonic sequence number bound into the MAC (and the job id), the session accepts only a higher sequence, final states are terminal.
- **Memory read on Windows**: a same-user process can read a shim's key from its memory (ReadProcessMemory). The SPEC lists this as out of scope; keep it
  out of scope but say so precisely and do not claim more than the boundary gives. Consider holding the key only for the shim's lifetime and zeroing it on exit.
- **SPEC wording**: the text says the key goes only to the session and the job's shim; that is false given B1/B2. Correct SPEC/DEVIATIONS to the real boundary
  after the fix.

## Nits
- Two log messages (`src/session.rs` near lines 2730 and 2802) contain long runs of spaces from a mangled line continuation.
- The refusal budget is shared across ids; consider per-id budgets.

## Held up
Refusal-flood and claim-flood tests pass; a resumed session rejects an edited record; the earlier round-2 attacks via forged status files are refused.
