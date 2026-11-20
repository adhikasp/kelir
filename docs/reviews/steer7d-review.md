# STEER-7 review, round 4 (steer7d) — verdict: FAIL (1 blocker, 3 should-fix, 3 nits)

Reviewer: fresh Claude Code session. Code under review: `8dcb5aa..1172a08` (round-3 fix, worktree `kelir-wt/steer7fix`). Windows 11 and Linux (the maintainer,
`kernel.yama.ptrace_scope=1`). New tests: `tests/review_steer7d.rs`; live scripts `scripts/live/steer7d_child.py`, `steer7d_live.sh`.

## Round-3 findings: closed

All verified on the real release binary, by the committed tests (`tests/review_steer7c.rs`, Windows and Linux) and a live drive in a Herdr pane (cheap model
ran `kelir bg run`, then the child-vantage script from a bash tool call):

- **B1** `bg-spawn` naming an existing job id is refused with no key material (`bg_spawn_naming_a_running_job_does_not_hand_out_its_key`; live: `respawn-named-id` ok=false).
- **B2** `bg-claim` answers `{id}` only (live: `state_keys=["id"]`); no verb reads a caller-named path (`a_claimed_key_cannot_aim_a_job_at_a_file_outside_the_session`).
- **Replay**: signed `seq`; an older record put over a final one is not believed after a resume (`a_resume_does_not_believe_an_older_signed_record_replayed_over_the_final_one`).
- **Final sidecar**: authenticated by the job's Ed25519 signature (id and `seq` are inside the signed bytes) plus `is_job()`/`settled()`; an unsigned or foreign-id sidecar is ignored (live: `planted-unsigned-final` forged_in_log=false).
  A FIFO planted as `<id>.final` does not hang the session (`a_fifo_planted_as_the_final_sidecar_does_not_hang_the_session`, Linux).
- Refusal/claim floods stay bounded; a refusal reveals only whether the session owns an id (ids are not secret).
- Suite: Linux HEAD `1172a08` 975/975 passed; Windows gate on HEAD 976 passed, 3 skipped (the only failure was my new test). Regressions in M4 bg, promotion, wake, orphan
  cleanup, STEER-5 interrupts and the TUI verbs: none. No leaked processes from my runs.

## Blocker

- **B1 — a refused `bg-spawn` writes the caller's whole environment and the session token into `events.jsonl`.**
  `src/session.rs:2095` (`refuse_verb("bg-spawn", &request.args, …)` → `log_verb` at `src/session.rs:2189`), fed by `SpawnRequest.env = envs::all()` / `.token`
  (`src/bg.rs:733`, `:1530`). Tool children inherit the provider keys (`src/tools/bash.rs` does not scrub), so any refusal `check_spawn` can produce (`cwd`
  not a directory, a `dir`/`out` outside `bg/`; trivially triggered by any token holder) puts `OPENROUTER_API_KEY` and `KELIR_TOKEN` in the log.
  Contradicts SPEC ("a key … is never written to the log", `docs/SPEC.md:453`) and the round-3 principle. Impact: a persistent, shareable file (logs are quoted in reviews and
  resumed) holds the provider key. Confirmed live with the real key (`refused-spawn-env-in-log`: provider vars and token present), then deleted. Test:
  `a_refused_spawn_does_not_write_the_callers_environment_or_token_into_the_log` (fails on Windows and Linux).
  Fix direction: never log `env`/`token` of a spawn (log a redacted copy: verb, dir, cwd only), and do not log `bg-spawn` args wholesale.

## Should-fix

- **S1 — `bg-spawn` trusts the caller's `kind`, `sock`, `token`, `job`, `call`.** `src/bg.rs:737-765` (`SpawnRequest::launch`) forwards them; `shim_main` hardens itself only
  `if args.kind == KIND_JOB` (`src/bg.rs:991`) and reports to `args.sock` (`:1213`).
  (a) A child naming its own listener as `sock` receives the shim's `bg` report with the job's **capability**
  (`a_spawn_cannot_send_the_jobs_capability_to_an_address_the_caller_chose`, fails on both OSes).
  (b) A child sending `kind: "x"` gets a shim that skips `harden_process`, so the job's **signing key** is readable by a sibling child on Windows and dumpable on Linux
  (`a_spawn_with_another_kind_still_gets_a_shim_closed_to_memory_reads`, `…non_dumpable_shim`); with that key it signs records for the job it spawned.
  Impact is bounded to the child's own job (ids/keys are per job), but it re-opens "a child learns a capability/key" by a path the round-3 fix did not close.
  Fix direction: the session sets `kind=job`, `sock` (its own), `token` (its own), `job` and validates/ignores `call` itself; accept from the request only `cmd`, `shell`, `cwd`, `env`.
- **S2 — the session process holds every job's private key in readable memory.** `Trust.jobs` (`Job`, `src/trust.rs:477`) keeps all keys for the whole session; only the shim is
  hardened. On Windows a same-user child reads the keys of **all** jobs from the session with the same 60-line script
  (`a_same_user_child_cannot_read_job_keys_from_the_sessions_memory`; live: `memscan-session` FOUND). On Linux the session is dumpable
  (`the_session_process_is_not_dumpable_while_it_holds_job_keys`); readable only where Yama is off (`ptrace_scope=0`), not on the maintainer.
  SPEC already declares this out of the boundary, so it is not a regression, but it makes the shim hardening protect little on Windows. Fix direction: apply
  `deny_same_user_memory_access` / `PR_SET_DUMPABLE=0` to the session too (it must keep the right to be killed by the harness), or keep the keys only in the shim and sign
  `cancel`/`lost` records some other way; otherwise say in SPEC that Windows shim hardening is partial.
- **S3 — wake text from a signed record is quoted as `from: job`.** Not new, but now the only forgery left is "signed by a key the child can reach" (S1b, S2): the child
  chooses `cmd`/`error`/`out` (inside `bg/`) of a record the model trusts more than tool output. Fix with S1/S2; no separate change.

## Nits

- `src/proc.rs` `deny_same_user_memory_access`: the SDDL literal ends in an embedded `\0` byte in the source; works, but a clean `encode_utf16().chain(once(0))` is safer.
- `bg-claim` is now dead (nothing sends it) but still spends the 256-id budget per call; a token holder can exhaust it and block `kelir bg run`. Remove it or give it its own budget.
- `bg-spawn` accepts an arbitrary `call` id (not checked against `live_calls` as `bg-claim` does), so a job's `bg_started.call_id` can name another call.

## Not tested / residual

- Handle inheritance: the key/capability pipe's read end is inheritable while the session starts the shim; a concurrent `CreateProcess` from another session thread could
  inherit it in a very narrow window. Not reproduced; worth `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`.
- Replay after a resume combined with deleting `<id>.final` can downgrade a result to `lost` (withhold, as documented).
- Elevated/SeDebug, root, kernel debuggers, edits to `events.jsonl`/the session dir are outside the documented boundary and were not attacked.
- `KELIR_GO_API_KEY` unavailable: Go path not live-verified.

## Live drive

`steer7d_live.sh` in pane `live-steer7d` (cheap model, `deepseek/deepseek-v4.1-flash-1017`): the model ran `kelir bg run "sleep 120"` then the child script. Result
`SUMMARY ["refused-spawn-env-in-log","memscan-session"]`; every other attack refused (named-id respawn, claim, forged report, unsigned final). Scratch state with the key-bearing log was deleted.
