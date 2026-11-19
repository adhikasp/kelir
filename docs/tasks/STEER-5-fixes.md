# STEER-5 fixes (from docs/reviews/steer5-review.md; verdict FAIL, 2 blockers)

Read the review in full first (it is in your worktree: docs/reviews/steer5-review.md), plus docs/tasks/STEER-5.md.

1. **B1 (merge)**: your branch predates STEER-4. FIRST `git merge main` into your branch (main now has STEER-4 `paused`,
   M5-A, STEER-4 fixes and docs) and resolve: unify the two `TurnEnd::Paused` variants into one that carries both the
   STEER-4 step-cap pause and the STEER-5 provider-exhaustion pause (one outcome name `paused`, a reason field), unify the
   Herdr status mapping (STEER-4: `paused` -> blocked? check `docs/DEVIATIONS.md` of both and keep ONE documented rule),
   resolve src/resume.rs, SPEC.md, DEVIATIONS.md conflicts, and make everything compile and pass before touching anything
   else. Commit the merge.
2. **B2**: `say --now`, `later`, `model`, `end`, `stop` and a signal must interrupt a retry/governor wait **immediately**
   (select on the verb channel and signals), apply per the verb's meaning, and answer truthfully ("applies":"now" only when
   it is). A `model` swap during a wait retries immediately with the new model (new governor slot, fresh budget).
3. **S1** rebuild/re-key the governor on a `model` swap and cover compaction summary requests; **S2** floor hints (>= 250 ms or
   the first jittered ceiling) so `Retry-After: 0` / past dates cannot hot-loop; **S3** the visible/NDJSON/`error` message
   carries a short reason (status + class), never the response body (bodies only in a debug-level blob file with secrets
   redacted if you keep them at all); **S4** make SPEC/DEVIATIONS match the code (and list remaining gaps honestly).
4. Nits: N1 use the response `Date` header for HTTP-date hints; N2 prefer the reset header closest to the limit that fired
   (document); N3 document that a failed summary is skipped, not retried (or retry it, your call, test it); N4 emit the done
   marker on the interrupt path.
5. Red tests from the reviewer (tests/review_steer5.rs or similar) must pass unweakened.

Gates: fmt, clippy -D warnings, `cargo test --no-fail-fast` (3 runs; known load flakes: review_m4 r18/r19, e2e_verbs are
pre-existing, see STEER-9), `bash scripts/remote-check.sh the maintainer kelir-verify-steer5fix`. Commit `steer5fix: ...` with explicit
paths; notes to docs/retros/steer5fix-impl-notes.md; final answer "steer5fix ready: <commit>".
