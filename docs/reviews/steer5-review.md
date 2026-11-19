# STEER-5 review (retry with backoff on provider errors, concurrency governor)

Reviewer: fresh QA agent. Worktree `kelir-wt/steer5`, branch `job/steer5`, reviewed
`82ad195..6023178` (the branch's real base; `9dd351f..HEAD` as named in the request also shows
STEER-4's merge as deletions, because the branch was cut before STEER-4 landed — see B1).

## Verdict: FAIL (2 blockers, 4 should-fix, 4 nits). The machinery is sound and the live drives pass; two things must be fixed before merge.

What is good, so the findings are read in proportion: classification, `Retry-After` (seconds, HTTP-date,
asctime), reset headers, full-jitter schedule with cap, the budget, pause/resume, the visible line, the TUI
countdown, `stop`/`end` interruption and the cross-process governor all behave as specified and are
independently reproduced below, on Windows and Linux. Footprint: release binary 6.4 MB.

## Findings

| # | Sev | Where | Finding | Proof |
|---|-----|-------|---------|-------|
| B1 | **blocker (merge)** | branch base | Branch is based on `82ad195`, before STEER-4 (`9dd351f`). `git merge-tree HEAD 9dd351f` conflicts in `docs/SPEC.md`, `docs/DEVIATIONS.md`, `src/resume.rs`, and **auto-merges `src/session.rs` into a file with two `TurnEnd::Paused` variants** (`Paused(u32)` from STEER-4 and `Paused(PauseReport)` here) and two `Ok(TurnEnd::Paused(..))` arms: it will not compile. STEER-4 also maps `paused` to Herdr `Blocked`; DEVIATIONS here says `idle`. Rebase onto `9dd351f`, unify the variant (e.g. one `Paused { steps/provider }` payload), pick one Herdr state, re-run all gates. | `git merge-tree --write-tree HEAD 9dd351f` then `git show <tree>:src/session.rs \| grep -n Paused` (lines 365 and 371) |
| B2 | **blocker** | `src/session.rs:2689` `take_interrupt`, `wait_for` (~2596) | The brief and SPEC say `stop`, `say --now`, `later`, `model`, `end` **and a signal** "take effect immediately" during a wait. Only `stop` and `end` do. `say --now` and `model` are pushed to the scheduler and run at the boundary *after the wait ends*; `kelir ctl … say --now` even answers `"applies":"now"`. With a 60 s `Retry-After` (or `retry_max_wait = 0` and a long hint, up to 7 days) a person's correction is ignored for the whole wait, and a `model` swap to a provider that is not rate limited still sits out the old provider's wait. A signal (Ctrl-C) still kills the process (implementer's known gap) instead of ending the turn `stopped`. | tests `say_now_during_a_wait_does_not_wait_it_out`, `model_during_a_long_wait_retries_at_once` (fail on Windows and Linux); live drive `now` below: no request for 12 s of a 60 s wait |
| S1 | should-fix | `src/session.rs:1649` `swap_model`, `:2474` | The governor is built once at start from the starting provider and never rebuilt on a `model` swap. After swapping to a governed provider the request takes **no slot**; swapping away keeps the old limit. Compaction summary requests (`!summary` guard) bypass the governor too, though they are provider requests on the same account. | test `the_governor_follows_a_model_swap` (state `locks/` stays empty) |
| S2 | should-fix | `src/retry.rs` `delay()` | A provider hint is returned verbatim, so `Retry-After: 0` (or a reset header `0s`, or a past HTTP-date) means a zero wait: a gateway that answers `429 Retry-After: 0` forever receives 200 requests in 0.65 s (309/s) before the safety cap pauses the turn. Floor the hint (e.g. at least the jittered first-step ceiling, or 250 ms) so "backoff" always spaces requests. | test `retry_after_zero_is_not_a_hot_loop`: `200 requests in 646ms` |
| S3 | should-fix | `src/provider.rs` (`http {code}: {body}`) → `error.message` | The brief says the `error` event carries a "short reason, never the body of secrets". The event's `message` is still `http <code>: <body up to 500 chars>`, so a gateway that echoes the request (or a 4xx that prints the key) puts the credential into `events.jsonl` and into NDJSON stdout. DEVIATIONS accepts this ("a credential can only appear in the session's own log") but NDJSON stdout is not only the log, and the brief is explicit. Redact the client's own key value from the message (cheap) or send only status + reason for retryable errors. | test `a_credential_echoed_in_a_body_never_reaches_events_or_output`: event contained `Bearer test-openrouter-key` |
| S4 | should-fix | `docs/SPEC.md`, `docs/DEVIATIONS.md` | Docs state things the code does not do: SPEC says `later`, `say --now`, `model` and "a signal" take effect at once during a wait; DEVIATIONS says the governor's slot wait is interruptible by `say --now` and that NDJSON carries the wait line "as a separate `retry` line" (in `--json` mode only the `error` event exists; the line goes to the TUI/stderr). Either fix B2 or correct the text, and list the signal gap and the summary-not-retried choice (N3) under DEVIATIONS. | read `show_wait` (`src/session.rs` ~2579): nothing is written to stdout in `--json` |
| N1 | nit | `src/retry.rs` `absolute()` | HTTP-date / RFC 3339 hints are measured against the local clock, ignoring the response's own `Date` header. A skewed local clock turns a 90 s hint into 0 (hammering, then S2) or into hours (pause). Use `Date` from the same response when present. Not exercised: system sleep / suspend (waits use tokio's monotonic timer). | code reading |
| N2 | nit | `src/retry.rs` `header_hint` | Takes the **largest** `x-ratelimit-reset-*`, i.e. a daily window can outweigh the per-minute one that actually produced the 429. Safe but may pause where waiting a minute would do. | code reading |
| N3 | nit | `src/session.rs:2457` | A compaction summary request gets a zero budget, so a 429 on it is not retried (M3 N4 "tried once"). The brief says provider errors are retried; this is defensible (failed summary = skipped compaction, not a failed turn) but is not in DEVIATIONS. I could not drive a 429 onto the summary request through the mock (the compaction did not engage in my scenario), so this is by code reading only. | code reading |
| N4 | nit | `src/session.rs` `stream` | `RetryingDone` is not emitted on the interrupt path; harmless because `TurnEnded` clears the TUI state, but a model swap + end races could leave a stale note. | code reading |

## Test pyramid (Windows, run three times)

* `cargo fmt --check`: clean. `cargo clippy --all-targets -- -D warnings`: clean.
* Run 1 (`cargo test --tests`, implementer's tree): **462 passed, 0 failed**: lib 307, e2e 16, e2e_m3 11, e2e_m4 10, e2e_m5 6, e2e_steer5 15, e2e_tui 16, e2e_verbs 23, others as before.
* Runs 2 and 3 (`--no-fail-fast`, with my review tests added): **470 passed, 5 failed**, the 5 being exactly my failing review tests (B2 x2, S1, S2, S3); identical in both runs. **Zero flakes** in any implementer suite across the three runs (review_m4 r18/r19, e2e_verbs all green every time).
* Linux (`scripts/remote-check.sh the maintainer kelir-verify-steer5-rev`, HEAD `6023178`): fmt OK, clippy OK, lib 312, all implementer suites green including the governor's two-process and killed-holder tests (`flock` path), and the same 5 review tests failing as on Windows.

My added tests: `tests/review_steer5.rs` (13): the 5 failing above, plus passing ones that confirm the
implementer's claims under attack: `end` during a wait exits at once; absurd `Retry-After` (`-5`,
`tomorrow`, 23-digit, `1.5`, `1e3`) fall back to the schedule with waits within the cap; a huge valid value
(`999999999`) pauses at once with one request; `x-ratelimit-reset-requests: 1s` is honoured (`retry_in_ms`
1000); 400/401/403/404/422 are never retried (one request; 401/403 may take the single credential re-read);
408/500/502/503/504/529 are retried; a free slot costs no latency; `kelir resume` after a provider pause
re-sends the identical fold and the turn ends `done`.

## Live drive (real binary, Herdr panes, scripts in `scripts/live/steer5_*`)

Mock/canned (`steer5_flaky_server.py`, no keys):

| Scenario | Result |
|---|---|
| `flaky`: 429 `Retry-After: 3`, 429 no header, 503, ok (`kelir run`, human output) | PASS. Lines on stderr: `kelir: rate limited (429): retrying in 3 s, attempt 1, say stop to cancel` / `… in 864 ms, attempt 2 …` / `kelir: provider error (503): retrying in 7 s, attempt 3 …`; request timestamps 3.07 s, 0.89 s, 7.02 s apart (matches `retry_in_ms` 3000/864/7005, ceilings 2/4/8 s); outcome `done`, exit 0; events carry `status`, `attempt`, `retry_in_ms`. |
| `stop`: 60 s `Retry-After`, `ctl stop` mid-wait | PASS. `turn_ended` outcome `stopped` **0.24 s** after the verb; one request only; exit 0. |
| `now`: same wait, `ctl say --now "…"` | **FAIL (B2)**. Ack says `"applies":"now"`; no second request and no `turn_ended` for the 12 s I watched out of the 60 s wait. |
| `pause`: budget 4 s, `Retry-After: 3` forever | PASS. 2 attempts, second error has no `retry_in_ms`; `turn_ended` outcome `paused`, `provider unavailable after 2 attempts, 3 s: rate limited (429); say continue to retry`; one-shot exit 0; `kelir resume` retried from the same fold (4 requests total) and paused again because the server still answered 429. |
| `cut`: SSE cut mid-body before any tool call | PASS. `kelir: provider error: retrying in 834 ms, attempt 1`, second request ok, outcome `done`. |
| TUI (`kelir` in a pane, 429 `Retry-After: 25`) | PASS. Screen via `hc read`: live line `» rate limited (429): retrying in 25 s, attempt 1, say stop to cancel` and status line `kelir working · rate limited (429): retrying in 25 s, attempt 1, say stop to cancel · 24 s left`, counting down; status stays `working`. (A desktop screenshot was taken but only showed an unrelated browser window over the pane; I deleted it rather than keep it. The `hc read` capture is the evidence.) |

Real OpenRouter (`deepseek/deepseek-v4.1-flash-1017`, through `steer5_overlap_proxy.py`, which records
request start/end and in-flight count, never headers or bodies), two real kelir processes each running a
three-tool task, 8 provider requests per run:

| Config | Peak requests in flight | Timeline |
|---|---|---|
| `[providers.openrouter] max_concurrent_requests = 1` | **1** | every `start` follows the previous `end` (s1 0.0 e1 1.2 s2 1.3 e2 3.2 s3 3.2 … e8 9.6); both processes exit 0, both tasks complete; lock file `…-api-v1-0` under `<state>/locks` |
| no knob (control) | **2** | s1 0.0, s2 0.0, e2 0.7, s3 0.9, e1 1.1 … |

PASS: serialisation is real across two OS processes on the live provider. The killed-holder case is
covered by the implementer's e2e test (and passes on Linux `flock` and Windows `LockFileEx`).

Hygiene: no key in any scratch file (grepped), the env file was never printed or copied; all processes
I started exited; two `kelir.exe` seen in `tasklist` belong to another worktree (`m5afix`) and were left alone.

## Not covered / not verified

* A real 429 from a real provider (not cheap to trigger): covered only by the canned server.
* System sleep/suspend and clock skew: code reading only (N1).
* Retry while a *resume* happens mid-wait (kill during a wait, then `resume`): not driven; the log holds only the `error` event, so the fold is intact, and the after-pause resume is tested.
* 429 on the compaction summary: not driven (N3).
* Signal during a wait: not implemented (part of B2).
* `kelir doctor` showing retry settings (brief marks it optional; STEER-2's doctor is not on this base).
* Go provider: not live-verified (no `KELIR_GO_API_KEY`); mock only.
