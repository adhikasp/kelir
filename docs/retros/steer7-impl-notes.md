# STEER-7 implementer notes (session-socket hardening)

Branch `job/steer7`, based on `main` at `e13662a`. Commits: `ca99415` (the feature), `c6a87e5`
(the Linux-gate fix in a `cfg(windows)` test), `5f7d88a` (a cancel in flight is not a lost job),
`5accaef` (the cancel marker's time to live), plus docs commits.

## What I did

- **`src/trust.rs` (new, no dependency).** The whole boundary, as a value the session owns:
  - a per-session token: 32 bytes from `/dev/urandom` (Unix) or `BCryptGenRandom` (Windows),
    hex; written to `<session dir>/token` owner-only and checked back by reading it;
  - `accepts(&Request)`: the socket half. A request with no token, or another session's token,
    is answered and its connection closed **before** it reaches the session's channel;
  - `owns(id)` / `check_job` / `check_spawn`: the job half — `bg` reports, `cancel` and
    `bg-spawn` only answer for ids this session created (in-memory registry, `bg/<id>.status`,
    or `commands/<id>/`), and every directory/out/status path in a request is canonicalised and
    must land inside the session's own `bg/`;
  - `resolve_inside`: `..`, a symlink out of the directory, `\\host\share`, `//host/share` and,
    on Windows, another drive are all refused. A path whose last component does not exist yet
    is resolved through its nearest existing ancestor, which is what makes checking a job's
    `out` file before the shim writes it work.
- **`src/perm.rs` (new).** The socket/pipe permissions, re-read rather than assumed: `0600` and
  a `stat` on Unix; on Windows the pipe is created with an owner-only SDDL and the DACL is read
  back from the handle (any ACE for a SID that is not this user is refused). It also holds the
  token file's Windows DACL (`SetNamedSecurityInfoW` by name, `GetNamedSecurityInfoW` to read it
  back).
- **`src/ipc.rs`.** `Request` carries `token`; the listener checks it (`trust.accepts`); the
  listener applies the descriptor and then re-checks the socket/pipe (`perm::check_socket`,
  `check_pipe`) before `session_started` is recorded. `request_with_token_file` (what `kelir ctl`
  uses) fills the token from `<session dir>/token`; `request_value` (what a `KELIR_SOCK`-only
  child uses) fills it from `KELIR_TOKEN`.
- **`src/session.rs`.** Owns the `Trust`; creates `bg/` at start; refuses a foreign job id or a
  spawn outside `bg/` and records the refusal as a `verb` event with boundary `rejected`;
  registers every job it starts. `Verb::Cancel` is applied by `apply_actions` — a new idle-loop
  step — so a `cancel` on an idle `--keep` session kills the job instead of waiting for a
  boundary that never comes.
- **`src/verbs.rs`.** `Verb::Cancel { id }`, `Verb::is_action`, `Scheduler::take_where`, and a
  `cancel` arm in `cli_args` so `kelir ctl <session> cancel <id>` actually sends `{"id": …}`
  (the positional word used to be dropped, and the request was refused as "missing the job id").
- **`src/bg.rs`.** The shim is started with `KELIR_TOKEN` in its environment (never on its
  command line: an argv is world-readable); it is not re-exported to the command, which inherits
  it. `cancel_in` runs the kill in this session's own `bg/` and returns its sentence.
- **`src/tools/mod.rs`, `src/tools/bash.rs`.** Every bash child gets `KELIR_TOKEN` next to
  `KELIR_SOCK`, so `kelir bg run|out|wait|cancel` and `kelir mcp` work from inside a call
  without being told anything else.
- **Tests.** `tests/e2e_steer7.rs`, ten tests against the scripted mock: a foreign job report and
  a foreign `cancel` are refused and the log says so; `bg-spawn` outside `bg/` is refused; a path
  escaping the session (including through a symlink) is refused; a request with no token never
  reaches the session (no `verb` event, no state change); another session's token is refused and
  is not echoed back in the error; a child of one session cannot cancel another session's job; a
  `ctl cancel` of an owned job kills its tree and logs `bg_settled` with state `cancelled`; `ctl`
  works through the token file and the secret appears in no log line, stdout or stderr; a tool
  child sees `KELIR_TOKEN` and `KELIR_SOCK`. Unit tests in `trust.rs` (token uniqueness and
  length, constant-time compare, `..`/UNC/absolute escapes, permission verdicts) and a Windows
  test in `perm.rs` that a file with the inherited ACL reads as too wide and a restricted one as
  owner-only.

## What was hard

1. **A Windows DACL cannot be written through a handle opened by `create`.** The first version
   opened the token file with `WRITE_DAC` and called `SetSecurityInfo`: `ERROR_ACCESS_DENIED`
   (5) for a file this process had just made, because a plain create does not ask for
   `WRITE_DAC`. `SetNamedSecurityInfoW` — same operation, taking the name — is allowed for the
   owner, so that is what the code does, and `GetNamedSecurityInfoW` reads it back the same way.
   The failure mode is worth remembering: `create` succeeding says nothing about the handle's
   rights.
2. **The `cancel` verb was a hole nobody had tested.** `cli_args` had arms for `say`, `stop`,
   `model` and `compact` and silently dropped everything else's positional words, so
   `kelir ctl <s> cancel <id>` sent `{}` and was refused as "missing the job id" — a `cancel`
   that never worked is easy to mistake for a `cancel` that was refused for security.
3. **A `cancel` is not a message, and the queue only knew about messages.**
   `take_turn_messages` drained every verb that applies at `turn_end`, and `cancel` applies
   "now", which `applies_at` answers `true` for every boundary. The first version therefore
   swallowed the cancel in the idle loop (it was taken, never applied). `Scheduler::take_where`
   plus `Verb::is_action` keeps actions in the queue, and the idle loop applies them.
4. **`bg-spawn` outlives its caller.** On Windows a `kelir bg run` inside a call sits in the
   call's Job Object (which must forbid breakaway, or the call could not be cancelled), so the
   shim has to be started by the session, from outside it. That means `bg-spawn` is the one verb
   that both carries paths and creates a process, so it needs the same registry check *plus* a
   directory check, and the refusal has to happen before anything is spawned.
5. **The Linux gate caught a test that believed in DOS path separators.** `..\..\elsewhere` is
   an escape on Windows and one harmless file name on Unix; the assertion is now under
   `#[cfg(windows)]` (and `trust::resolve_inside` still refuses it there, which the test proves).
   This is the class of bug the Linux gate exists for.
6. **Not logging an unauthenticated request is a deliberate deviation.** The brief asks for a
   `verb` event marked refused; that is right for a *caller of this session* (a foreign job id
   from a child) and wrong for a stranger, for whom it would be a log-write primitive. So the
   listener answers and drops unauthenticated requests before the session sees them, and the
   `rejected` events exist for the authenticated-but-wrong cases. Written up in DEVIATIONS.
7. **The last real bug came from a test that was flaky only sometimes** — the `ctl cancel`
   e2e failed about half the time with `bg_settled state:"lost"` for a job it had just
   cancelled, and the first instinct ("load flake, rerun") was wrong. Two things were wrong and
   neither was the test: (a) `cancel_at` kills the shim before it writes `cancelled`, so for up
   to `CANCEL_WAIT` the file says `running` with a dead shim, and `Status::observed` — by design,
   since M4 — calls that `lost`; (b) `note_bg` recorded the watcher's *snapshot*, so a report
   taken while the shim lived was recorded after it died, and the (correct) `lost` reading of a
   stale snapshot overwrote everything. Fixed with the `bg/<id>.cancel` marker (readers say
   "running" while a cancel is in flight, and the marker expires so a killed cancel cannot pin a
   dead job), the re-read in `note_bg`, and `observed_in` used by `list`/`wait`/`out --follow`
   too. I found it by writing the raw status and marker state to a file from the session when
   the debug variable was set — printf-debugging through a child process' log, which is what the
   harness gives you for a process you do not control.

## Where the prompt, the tools or the harness slowed me down

- **The spec never said how a client authenticates.** "Reaching the socket is the whole of the
  authorisation" was the old behaviour, so the brief's "such as `KELIR_TOKEN`" was the only
  design input, and two of the three routes a client can take (a tool child, a bare process that
  inherited only `KELIR_SOCK`, `kelir ctl` from a terminal) needed their own answer. It went into
  SPEC.md as *Who may drive a session*, and DEVIATIONS says where the choice was mine.
- **`cargo test` cannot be trusted to report a `cfg` mistake.** Everything on Windows passed with
  the DOS-path assertion in place; only the remote Linux run saw it. Running the Linux gate
  before believing a green Windows run is not optional for anything path-shaped.
- **The first version of the tests failed for unrelated reasons** (a job's output lives in a
  `commands/<seq>-<step>-<n>` dir, not `commands/<call_id>/`, so the test had to read `out_path`
  from the `tool_settled` event; and a session's temp dir is gone once `session.end()` returns,
  so the log must be read before it). Neither is a product bug; both cost a debug cycle.
- **`docs/reference/` does not exist yet** (STEER-11 introduces it), so the documentation went
  into SPEC.md and DEVIATIONS as the repo does today.

## Gates

- Windows: `cargo fmt --check` clean, `cargo clippy --all-targets -- -D warnings` clean,
  `cargo test --no-fail-fast` 651 passing over 25 suites, 0 failed (`e2e_steer7` 10/10, and the
  cancel test 8/8 in a row after the fix; it was ~50 % red before); no `kelir` process left
  behind (`Get-Process kelir` → 0).
- Linux (`bash scripts/remote-check.sh the maintainer kelir-verify-steer7`, HEAD `5accaef`): fmt clean,
  clippy clean, 653 passing, 0 failed, exit 0.
