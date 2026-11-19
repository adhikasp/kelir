# STEER-5 review, round 2 (steer5fix + steer5fix2), job steer5b

Reviewer: fresh QA agent. Worktree `kelir-wt/steer5`, branch `job/steer5b`, diff `9bb5ca4..7e74471`.

## Verdict: PASS WITH FIXES (0 blockers, 1 should-fix, 3 nits)

Every round-1 finding (B1, B2, S1–S4, N1–N4) is fixed and re-verified on the real release binary. The Linux SIGINT
race is gone (15/15). The only open item is test flakiness on Windows under load in `e2e_verbs`, one case of which
is a test that the new retry config made timing-dependent.

## Findings

| # | Sev | Where | Finding | Proof |
|---|-----|-------|---------|-------|
| F1 | should-fix (tests) | `tests/e2e_verbs.rs:890` `stop_during_a_provider_backoff_is_obeyed_at_once`; `tests/support/mod.rs:202` `TEST_RETRY` | The test starts a session with `retry_base_ms=200, retry_cap_ms=400, retry_max_attempts=3` against three 500s, then spawns `kelir ctl stop`. On a loaded Windows box the ctl process starts after the backoff (and the 3 attempts) is over, so the turn ends `paused` (the truthful outcome) and the assertion `outcome == "stopped"` fails. It is a test race, not a product bug (product is correct: stop 0.04–0.09 s in the live drive). Same file, same load: `say_now_stops_the_running_call_within_two_seconds` once took 4.0 s (line 129) and `end_ends_the_session` once got ctl exit 1 (line 538). | Windows, `cargo test --test e2e_verbs`: 1 failure in the full run, then 3 failures in 16 further runs (stop_during_a_provider_backoff x2: `left: "paused" right: "stopped"`; say_now x1; end_ends x1). Linux: 10/10 clean. Fix: give this test a long `retry_base_ms` (or a gated mock) so it never races the spawn. Belongs with STEER-9's flake inventory. |
| N1 | nit | `src/session.rs` ack for `model` | During a wait the ack says `"applies":"next_step"` and `state.model` still reports the old model, although the swap took effect at once (retry sent 0.07 s later on the new model). Not false, but weaker than `say --now`'s `"now"`. | live `model` scenario below |
| N2 | nit | `src/retry.rs` floor | `Retry-After: 0` / a past date for ever gives 4 req/s for 60 attempts (15 s) before the pause. Bounded and documented; a floor of one jittered step would be friendlier to a real gateway. | hint0: 60 requests in 20 s, min gap 259 ms |
| N3 | nit | docs / brief | The brief says `later` "takes effect immediately"; the code (and SPEC) keep its boundary (`applies:"turn_end"`, wait not cut). The ack is truthful and SPEC says so; DEVIATIONS should name this against the brief's line. | live `later` scenario |

## Pyramid

* `cargo fmt --check`: clean. `cargo clippy --all-targets -- -D warnings`: clean.
* Windows `cargo test --no-fail-fast`: lib 328, e2e 16, e2e_m3 11, e2e_m4 10, e2e_m5 10, e2e_steer4 6, e2e_steer5 17,
  e2e_tui 18, e2e_verbs 22+1 failed (F1), review_m0 8, m1 5, m2 2, m3 11, m4 19, m4b 7, m5a 20, steer1 6 (+2 ignored),
  steer4 9, steer5 13 (the round-1 review suite, all green now: B2 x2, S1, S2, S3 pass). Everything else green.
* Linux gate `scripts/remote-check.sh the maintainer kelir-verify-steer5b-rev`: fmt, clippy, lib 333 and all suites green
  (exit 0). `e2e_steer5` 15 consecutive runs on Linux: **0 failures** (19 tests each); `signals` unit tests 5/5;
  `e2e_verbs` 10x on Linux: 0 failures.

## Live drive (real release binary, canned server `scripts/live/steer5b_server.py` + driver `steer5b_drive.py`)

A 60 s `Retry-After` wait (`retry_in_ms: 60000`), then the verb. Windows in a Herdr pane, Linux via `hc exec --on the maintainer`
(debug build of the same commit).

| Verb | Windows | Linux | Ack truthful? |
|---|---|---|---|
| `stop` | outcome `stopped` 0.09 s | `stopped` 0.01 s | `applies:now`, yes |
| `say --now` | 2nd request 0.05 s, user text in fold, turn `done` | 0.03 s, `done` | `applies:now`, yes |
| `model test/m2` | 2nd request 0.07 s with `model: test/m2`, `done` | 0.01 s, test/m2 | `applies:next_step`, true but weak (N1) |
| `later` | wait NOT cut (0 requests after 3 s, no turn_ended); queued text ran after `stop` as the next turn | not run | `applies:turn_end`, yes |
| `end` | process exit 0 in 0.91 s, turn `stopped`, 0 extra requests | exit 0 in 0.03 s | `applies:now`, yes |
| SIGINT | n/a (cannot be sent safely to one pid; unit-tested) | `stopped` in 0.02 s, `--keep` session alive, then `end` exit 0. **15 runs with signal delay 0 / 0.3 s / 1 s (before, at and well into the wait): 15/15 stopped** | n/a |
| SIGTERM | n/a | driver run printed the same shape (stopped) | n/a |

Hints (`Retry-After` forever, 20 s window): `0` -> 60 requests, every `retry_in_ms: 250`, min gap 259 ms, then
the cap pauses; past HTTP-date -> identical (253 ms); `-5` -> 5 requests, schedule with jitter (132, 3125, 6797 ms...);
`soon` -> 6 requests on the schedule. No hot loop (floor 250 ms holds), N2 above.

S3 leak: a gateway echoing the Authorization header: on a retryable 429 the error is `http 429: rate limited`, key in
**no** file under the state dir, stdout NDJSON or stderr (searched byte-wise); on a 400 refusal the body is kept but
redacted: `http 400: provider refused: echo: Authorization=Bearer [redacted]`, key again in no file.

Tool-started: a stream that sends a bash tool-call head then cuts: **1 request only**, no retry, turn `error`
(`stream ended before the model finished`), tool did not run (`tool_ran.txt` absent). Retry of a text-only cut was
covered in round 1 and by e2e.

Governor on the real OpenRouter cheap model (`deepseek/deepseek-v4.1-flash-1017`, two real kelir processes, proxy
recording in-flight): `max_concurrent_requests = 1` -> 8 requests, **peak in flight 1**, both exit 0; control without the
knob -> peak 2. (Swap-time governor re-keying and summary coverage are covered by the e2e/review suites only, not live.)

Regression, live on the real model: STEER-4 `--max-steps 5` pauses (exit 0), `say continue` resumes with a fresh count
(10 steps over 2 turns, no call re-run); `kelir resume` after pause continues 4 -> 8 -> 12 steps and ends `done`. Single
`paused` outcome with `reason` (steps / provider) is consistent in SPEC/DEVIATIONS and the Herdr mapping (`blocked`
for both; `e2e_tui` asserts it). M3 resume, compaction and the TUI banner: covered by the green `e2e_m3`, `review_m3`,
`e2e_tui`; no new screenshot was taken (not re-driven by hand).

Hygiene: env file never printed or copied; only my pids were stopped (the driver terminates its own children by
handle); no leftover kelir or canned-server processes of mine (the `kelir-s2` processes belong to another job).

## Not covered

* Windows console Ctrl-C/Ctrl-Break delivery to one pid (cannot be done without broadcasting; unit test only).
* 429 on a compaction summary and a governor re-key on a live model swap: tests only.
* Go provider: not live-verified (no key).
