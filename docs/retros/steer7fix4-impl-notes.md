# steer7fix4 implementation notes (steer7d review: 1 blocker, 3 should-fix, 3 nits)

- **B1**: `verbs::loggable_args` is the only path from a request's `args` to the log (`Session::log_verb`):
  a whitelist per verb; `bg-spawn` logs `dir`, `cwd`, `env_vars` (a count). Generic canary test:
  `tests/steer7fix4.rs` (every verb, canary in `token`/`key`/`env`/unknown fields; log, stdout, stderr, answers).
  Provider keys: the shim removes `OPENROUTER_API_KEY` and `KELIR_GO_API_KEY` from the environment of every
  command it starts (`credentials::PROVIDER_KEY_ENVS`); git/cargo/`kelir` still run. Cost: a nested `kelir run`
  from a bash call needs its key on the command line. Custom `key_env` names are not scrubbed (they are the
  user's own variables). Documented in SPEC and DEVIATIONS.
- **S1**: `SpawnRequest::pinned` (session side): kind=job, own `sock`, own token, `job`/`id` cleared (the session
  names them), `call` kept only if in `live_calls`, `env` filtered (`filter_spawn_env`). The shim hardens whenever it
  holds a key, whatever `--kind`.
- **S2**: chose to harden the session process (`bg::harden_process` before `Trust::create`) rather than move keys.
  Windows: Owner Rights DACL; Linux: `PR_SET_DUMPABLE=0`. SPEC/DEVIATIONS say what protects what.
- **Nits**: SDDL literal is now `encode_utf16().chain(once(0))`. `bg-claim` is kept (the committed round-3
  test still calls it) but has its own 16-id budget (`Trust::claim_ack`) and cannot touch `CLAIM_BUDGET`.
  `call` is validated in `bg-spawn`. `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`: NOT done (needs raw `CreateProcessW`
  in place of std/tokio `Command`; not straightforward), the narrow inheritance window remains.
- **Test changed** (flag for the orchestrator): `a_spawn_cannot_send_the_jobs_capability_to_an_address_the_caller_chose`
  asserted `heard.is_some()` ("the test is vacuous otherwise"), which can only be true while the bug exists. With
  `sock` pinned nothing may connect; the guard is now "the spawned job wrote its status file". The no-leak
  assertion is unchanged.
