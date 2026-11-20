# steer7fix3 implementation notes

Brief: `docs/tasks/STEER-7-fixes3.md`; review: `docs/reviews/steer7c-review.md`.

- **B1/B2.** Secrets never leave the session/shim. `kelir bg run` (with a session) now sends one `bg-spawn`
  (command, shell, cwd, its environment, token); the session mints id/capability/key, writes the
  `cmd` script file itself, starts the shim with the client's environment (`env_clear` + the
  client's pairs) and passes key + capability on the shim's stdin pipe (2 lines). Answer: `{id}`.
  `bg-spawn` naming an id is refused in `trust::check_spawn`; `bg-claim` answers `{id}` only. The
  shim sends the `bg` report itself (it holds the capability); the client no longer notifies.
  The no-session path (bare shell) is unchanged apart from not touching the session.
- **Replay.** `Status.seq` (signed, so bound to the id's key and record). `write_with` takes a
  strictly increasing value (clock nanos, process-wide max+1). Settled records are also written to
  `<id>.final`; `note_bg` prefers a Good final record, and keeps `seen_seq` per id. Replays can
  only withhold. `e2e_steer7sig::a_resume_rejects_a_status_edited...` now asserts the edit
  (exit 77) never reaches the log and that any settle is the shim's own (exit 0), because the
  sidecar legitimately settles the job.
- **Memory read.** The scan test did find the key on Windows. The Windows shim now sets an Owner
  Rights DACL on itself (`proc::win::deny_same_user_memory_access`); the scan no longer opens it.
  Linux: `PR_SET_DUMPABLE=0`. Key/capability are overwritten on exit (best effort; copies made by
  the allocator/stdin buffer are not chased). Elevated/SeDebug/root stay out of scope (SPEC, DEVIATIONS).
- **Nits.** Two mangled log strings fixed. Per-id refusal budgets not done.
- **Drift fixes from the merge of main.** `cancel` verb row, `tool_started.dir`, `bg_started.pubkey`,
  `KELIR_TOKEN` in `envs::VARS` (`envs::all()` for the environment hand-over); docs regenerated.
