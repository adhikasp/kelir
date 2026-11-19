# STEER-9 — de-flake the e2e harness under load

Evidence (2026-11-19, a Linux box, 2-3 kelir workers + cargo builds running): the same suite fails different tests on
different runs and passes alone. `e2e_verbs` fails ~1 in 5 runs on main (`model_swap_can_change_the_provider`,
`say_mid_turn_is_in_the_next_request`, `stop_during_a_provider_backoff_is_obeyed_at_once`, `a_stopped_call_reports_its_partial_output`,
`model_swap_applies_at_the_next_step`); `e2e_m5::an_edited_instruction_file_is_a_tail_message...` timed out at 32 s
(tests/support/mod.rs:708); `review_m4::r12_a_graceful_exit...` ("end was refused") and `r18` marker scans failed under load
earlier. None reproduce alone. A flaky gate trains everyone to ignore failures, which hides real ones.

## Build
- Inventory every fixed `sleep`/timeout/poll in tests/support and the suites; replace with **event-driven waits**
  (wait for a specific event in `events.jsonl`, a socket reply, a file, a process exit) with a generous ceiling
  (>= 60 s) that only bounds a hang, plus a diagnostic dump (events seen, pids, last N log lines) on timeout.
- Remove ordering assumptions about concurrent verbs/requests that depend on scheduler luck; assert invariants
  ("each message reaches some request, in order per sender") not timings.
- Add a **load stress mode** to the gates script: run each suite under artificial CPU load (N busy threads) and 3x
  in a row; the milestone gate requires 3/3 green. Report per-test failure rates in docs/reviews/FLAKES.md.
- No product behaviour change unless a flake proves a real race (then a red test first, and fix it).
- Windows + Linux. Gates as AGENTS.md; `cargo test --no-fail-fast` everywhere (a failing suite must not hide the rest).
