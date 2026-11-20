# STEER-7 fixes, round 2 (from docs/reviews/steer7b-review.md; verdict FAIL, 1 blocker, 3 should-fix)

Worktree kelir-wt/steer7fix (branch job/steer7fix, HEAD 06e97ac + review commits). Read docs/reviews/steer7b-review.md and
tests/review_steer7b*.rs (3 red tests prove the blocker; make them pass unweakened).

**Blocker**: the socket is hardened, but the session's `bg/*.status` file watcher still trusts files: a tool child (same user, in the session dir) can
write `bg/<id>.status` for an id the registry holds and the session logs forged `bg_started`/`bg_settled` events and a wake `user` message from `job`.
A file is not ownership evidence. Design to implement:
- Status records are **authenticated**: the shim writes each status record with an HMAC (SHA-256) over the canonical record keyed by the **per-job
  capability** (which the child never holds; check precisely how the capability reaches the shim: it must NOT be in argv/env readable by a same-user
  tool child, e.g. pass it over an inherited pipe/handle or a short-lived 0600/owner-ACL file the shim deletes after reading, never logged).
- The session verifies the MAC on every status read (watcher, `bg ls`, wait, resume folding); an unsigned or badly signed file is ignored and recorded
  once per id as a refused `verb`-style event (rate-limited).
- Resume: the registry and capabilities are rebuilt from the log; the child can append to `events.jsonl` only if it can write the session dir: make the log
  tamper-evident enough for the claimed threat model or state the limit honestly (same-user code that can write the session dir can forge anything
  unless the log is protected; if only an owner-ACL/0600 dir separates it, say that the boundary is "other users and sandboxed/unprivileged code", not
  same-user code) and correct SPEC/DEVIATIONS (it currently claims "a file is never ownership evidence", which was false).
- The 3 should-fix items in the review: fix them (list in the review).
Rerun every attack script under scripts/live (steer7_attack.py, steer7b_*): all refused. Gates: `bash scripts/gate.sh --linux=<machine from your prompt>`.
Commit `steer7fix2: ...` (explicit paths); notes docs/retros/steer7fix2-impl-notes.md; report with `hc send t1x "steer7fix2 ready: <commit>"`.
