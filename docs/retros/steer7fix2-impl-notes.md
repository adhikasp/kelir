# STEER-7 fixes, round 2 — implementation notes

Brief: `docs/tasks/STEER-7-fixes2.md`. Findings: `docs/reviews/steer7b-review.md` (FAIL: blocker B1', should-fix
S-A, S-B, S-C). The three red tests in `tests/review_steer7b.rs` pass unweakened.

## 0. Merged `main` first

`git merge main` into `job/steer7fix`; one conflict, `docs/DEVIATIONS.md` (both sides appended a section), kept both.

## 1. B1' — a status file is evidence only if the job's shim signed it

The watcher, the resume pass and the promotion pass all end in `Session::note_bg`; it was the one place that read a
`bg/<id>.status` and believed it. It now asks `Trust::seal(&status)`:

- **Ed25519, not HMAC.** The first cut was an HMAC under a per-job secret and it broke the existing contract that
  `resume` folds in a job whose shim outlived a killed session with its real exit code (`e2e_m4
  a_promoted_job_survives_a_killed_session_and_resume_folds_it_in`): a symmetric key dies with the session, and a key
  on disk is a key a tool child reads. So each job (and each bash call) gets a key pair from `ring` (already compiled
  for rustls, so the only change is the `ring = "0.17"` line in `Cargo.toml`). The private half lives in the session's
  memory and in that job's shim; the **public** half is in the registry and in the log (`bg_started.pubkey`, a new
  optional field), so a resumed session adopts it and verifies the survivor's records.
- **How the private key reaches the shim: an inherited pipe** (`std::io::pipe`, written in `shim_command` before the
  shim exists, `--key-stdin` tells the shim to read one line). Not argv, not the environment, not a file. The shim
  holds it in memory and does not pass it to the command (`KELIR_JOB_CAP` no longer exists). This also answers S-A:
  `/proc/<pid>/environ` and the PEB hold nothing.
- **What is signed:** the record without `sig`, in struct field order (`Status::sealed_bytes`), so a rewritten file's
  key order does not matter; `id` is inside it, and `note_bg` also requires `file.id == stem`, so a valid record of
  job A copied to `B.status` is refused.
- **Verdicts (`trust::Seal`):** `Good` is folded in; `Unowned` is ignored as before; `Unsigned`/`Forged` are ignored
  and recorded once per id as a refused `verb` event (`Session.refused_status`, then the existing 32-event budget);
  `NoKey` (owned, no public key: an old log, or a call that was never promoted) reports `lost` once the file says it
  stopped, built from the log's own `bg_started` (cmd, out_path) — the file's `cmd`/`out` are never used for it.
- **The snapshot is held to the same rule.** The old fallback ("file vanished, use the report") would have let a
  child write a forged file and delete it before the re-read; the snapshot now has to verify too.
- `out` can no longer lead anywhere: a record that verifies came from a shim, and a shim's `out` is what the session
  or `bg run` gave it (`bg run` uses `bg/<id>.out`).
- Plumbing: `bg-claim`/`bg-spawn` answers carry `key` beside `capability`; `Launch.key` replaces `Launch.capability`;
  `Ctx.trust` replaces the vestigial `Ctx.capability` so a bash call fetches its own key; `cancel_in` signs the
  `cancelled` record with the session's key.

## 2. S-A, S-B, S-C

- **S-A:** the secret is no longer in the environment (above). SPEC says plainly that a debugger or
  `ReadProcessMemory` on the shim reads it.
- **S-B:** SPEC (*Who may drive a session*, *Background*) and DEVIATIONS no longer say "a file is never ownership
  evidence" or that tool children cannot impersonate a shim without qualification. They state the boundary: other
  users, other sessions, stray processes and the model's own tools acting through the files and socket kelir exposes;
  **not** same-user code that reads another process's memory or rewrites `events.jsonl`/the session dir on purpose
  (a forged `bg_started.pubkey` or `tool_started.dir` is adopted by a resume). The `call_<hex>` claim is corrected: a
  call's id is `<seq>-<step>-<n>`, shared between sessions, harmless because each has its own `bg/`, registry, keys.
- **S-C:** `steer7_attack.py child` picks a victim id the attacker session does not own (`b2` was a false positive),
  and has a new `a5` that rewrites an owned call's status file unsigned and with a garbage signature and checks
  nothing reaches the log.

## 3. Tests

- `tests/e2e_steer7sig.rs`: the shim signs; neither the capability nor the key is in `environ`/`cmdline` (Linux);
  an unsigned and a self-signed rewrite of a running job's status settle nothing and log exactly one refusal;
  `bg_started` carries a 32-byte public key and a resume rejects a surviving shim's record edited after the fact.
- Unit: seal verdicts (good, edited, stranger's key, record copied across ids, unowned), adoption under the logged
  public key, a call has a key and no capability.
- Green: `review_steer7b` (3), `review_steer7`, `e2e_steer7`, `e2e_m4`, `review_m4` (`r19` is a process-name grep and
  flakes under parallel load; it passes alone).

## 4. Known gaps

- `kelir bg ls|out|wait|cancel` in a terminal or tool child do not verify (no key, view only); a terminal
  `bg cancel` writes an unsigned `cancelled`, so the session reports that job `lost`.
- A child can delete or roll back a status file; a signature does not stop that.
- N-a (`CLAIM_BUDGET` shared by claim and spawn) and N-b (one global refusal budget) are unchanged.
- The live scripts that need a real model (`steer7b_live.sh`) were not run here (no key in this worktree).
