# STEER-5 implementer notes (retry with backoff on provider errors)

Branch `job/steer5`, based on `main` at `82ad195`. Commits: `9ee2f21` (retry
policy), `3675458` (governor), `3bbab9b` (provider error shape), `5cf065b`
(the loop), `1c200ed` (tests), `43e8b07` (docs), plus the test-only deflake
fixes and `0339e3b` (event fields).

## What I did

- **`src/retry.rs` (new, no dependency).** The whole policy as pure functions:
  the status classification, `Retry-After` (delta seconds and HTTP-date, both
  IMF-fixdate and the obsolete asctime form), the reset headers in every shape
  I could find documented (Go-style `6m0s`, epoch seconds, epoch milliseconds,
  HTTP-date, RFC 3339), full-jitter backoff with an injectable RNG, and a
  per-request waiting budget. `time` (already a dependency) does the date
  maths; the jitter is a five-line xorshift.
- **`src/governor.rs` (new).** Opt-in `max_concurrent_requests` per provider:
  N advisory OS file locks under `<state root>/locks/`. `flock` on Unix,
  `LockFileEx` on Windows.
- **`src/provider.rs`.** `ProviderError` now carries the status, what the
  headers asked for, and whether a tool call of that step had already been
  streamed (which makes a mid-stream failure non-retryable).
- **`src/session.rs`.** `stream` waits, re-sends, and pauses when the budget is
  spent. `TurnEnd::Paused` mirrors the STEER-4 outcome shape.
- **`src/config.rs`.** `retry_base_ms`, `retry_cap_ms`, `retry_max_wait`,
  `retry_max_attempts`, `[providers.<n>] max_concurrent_requests`.
- **`src/tui.rs`.** A `Retrying` event with a countdown in the status line, a
  `Paused` phase, and the wait's line in the live area.

## What was hard

1. **"Unlimited attempts inside the budget" is not implementable as stated.**
   The budget is spent in *waiting*. A provider that answers `429` in 5 ms with
   no `Retry-After` and a `retry_base_ms` of 0 can be asked thousands of times in
   a second and never spend the budget. I added a finite cap (`MAX_ATTEMPTS`,
   200) and a config knob for a smaller one, and documented the deviation. This
   is the one place I think the brief is wrong rather than merely
   underspecified.
2. **"The retry uses the new model" changes the boundary model.** A `model`
   verb applies at `before_step`; a retry is a fresh request, so the retry path
   has to run that boundary. Doing that on the *first* attempt as well swallowed
   a verb `drive_turn` had already applied (it showed up as a `paused` turn in
   `say_mid_turn_is_in_the_next_request`). The fix is a flag: apply the boundary
   only when `attempt > 1`.
3. **`File::try_lock` needs Rust 1.89 and the MSRV is 1.88.** Clippy's
   `incompatible_msrv` caught it, and I wrote the two platform calls out by hand
   the way `ipc::lock` already does. That also meant adding two `windows-sys`
   features, a `Cargo.toml` edit that will conflict with any other job touching
   that list.
4. **The test harness had no way to send a header.** `Reply::Http` was body
   only, and `Retry-After` is the whole point of the first test. `HttpWith` and
   the in-flight accounting were added to `tests/support/mod.rs`, a shared file
   other jobs edit. I kept the additions appending, and the per-test-config
   change in one function (`test_config`), for that reason.
5. **Windows flakes that are not mine.** `ctl end` racing the process exit, a
   timing flake in `say_now_stops_the_running_call_within_two_seconds`, and the
   `notify_waiters`-lost-permit gate in three tests all fail on my base commit
   too. I fixed what was cheap (the notifications, the `end` ack, waiting for a
   request to arrive) and left the rest: they are M1/M4 test hygiene, not
   STEER-5.

## Where the prompt or the tools misled me

- **There is no signal handling to hook.** The brief lists "a signal" as a
  control that takes effect immediately during a wait. A `SIGINT` kills the
  process today (there is no handler, and adding one is a different job). The
  *interruptible wait* is implemented and tested via `stop`/`end`; "a signal
  ends the turn `stopped`" is not, because a signal ends the process. It is in
  Known gaps.
- **The brief's "attempts are unlimited" cost me two full `cargo test` runs**:
  the old `exhausted_retries_end_the_turn_with_exit_1` waited out 30 minutes of
  budget. If the brief had said "the test config must scale the budget down", I
  would have written `test_config` first. It is in the harness now, with a
  comment saying why.
- **`bash` heredocs through this tool ate backslashes** (`\n` inside a Rust
  string literal became a real newline, three times). Writing files with the
  `edit` tool, or with Python's `io`, was reliable; `cat <<'EOF'` was not, even
  quoted. Worth knowing for the next Rust-heavy job.
- **Two `cargo test` runs were still alive as background jobs when I started a
  third**, and one held the test binary's `.exe` open so the next link failed
  with `LNK1104`. I killed them by exact PID (never by image name) and used
  `timeout` on every run after that.

## Known gaps

- A signal (Ctrl-C to a headless `kelir run`) still ends the process; it does not
  end the *turn* as `stopped` mid-wait. There is no signal handler to hook yet.
- The governor counts requests, not tokens: it will not stop one session from
  consuming a whole token-rate limit. The brief's purpose — several local
  workers on one account — is served.
- `paused` is added minimally as STEER-4's outcome (see DEVIATIONS.md); if
  STEER-4 lands first, the merge is one variant and one arm.
- Windows's `LockFileEx` path is exercised by the governor's unit tests and the
  two-process e2e test, which is the same coverage `flock` gets.
