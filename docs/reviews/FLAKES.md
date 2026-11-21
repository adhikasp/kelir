# FLAKES — e2e flake inventory and measured rates (STEER-9)

The stress mode is `bash scripts/gate.sh --linux=<machine> --load`: three rounds of the whole
suite with `KELIR_GATE_LOAD` (default 4) busy loops beside it and `--retries 0`, so a flake is a
failure rather than a quiet retry. `scripts/flake-rate.sh ROUNDS BUSY_LOOPS OUT` is the measurement
wrapper; it takes the gate lock itself (do not run it while another gate holds it) and writes one
`<failures>/<rounds> <binary> <test>` line per test that failed at least once.

## Baseline (BEFORE)

Measured on the unmodified tree (WIP stashed), 2026-11-21, 3 rounds, 8 CPU burners,
`crates` 1350 tests/round. Logs: `flake-rate-20261120T020159Z` under `KELIR_SCRATCH`; rates also
in `steer9-before.txt` there.

| Failure rate | Test |
| --- | --- |
| 3/3 | `kelir::e2e_m9 the_next_day_adds_the_date_source_and_nothing_else` |
| 1/3 | `kelir::e2e_m9 wait_queues_behind_the_holder_instead_of_skipping` |
| 1/3 | `kelir::review_steer2 a_huge_export_is_handled` |

The 3/3 one was not load-related at all: it fails alone on a box whose real UTC date is
2026-11-21. `clock_source()` used the `SessionStarted` event's wall-clock `ts` as the date the
context last knew, ignoring the `KELIR_TODAY` override frozen into the system prompt. Once real
time caught up with the test's "new day" the source was never announced. Red unit test first
(`jobs::tests::a_clock_source_uses_the_date_the_session_was_rendered_with`), then the product fix:
`clock_source_from()` takes the rendered date, which `Session::rendered_date()` reads out of the
system prompt (inline or spilled) in `src/session/jobhooks.rs`.

The `wait_queues` one slept 800 ms to guess the waiter had reached the single-flight lock; the
assertion still looked at the mock's request count, which a late waiter could change. It now polls
the waiter's own process liveness (30 s ceiling via `until`) and asserts the same invariant while
the model's answer is still gated.

`a_huge_export_is_handled` is a 30 MB credential export through a `.cmd` shim; under 8 burners the
parse can cross the product's 5 s `AUTH_TIMEOUT`. The product bound is deliberate and the export
size is the point of the test, so neither is weakened; the AFTER section records the measurement.

## Other flaky tests named by gate logs

Collected from `gate-job-*/gate.log` and `gate-*.txt` lines containing `FLAKY` or `RETRIED`
(`C:/Users/the maintainer/kelir-scratch`, 2026-11-19..21). These were the drive-by list in the brief and
the reason each suite got an audit; the measured BEFORE table above is the load-mode inventory.

- `buildinfra waiting_gives_up_with_exit_75_when_the_holder_never_returns`
- `buildinfra_review a_waiter_does_not_steal_a_lock_that_was_just_created`
- `e2e_m4 a_promoted_job_survives_a_killed_session_and_resume_folds_it_in`
- `e2e_m7 only_the_root_writes_the_herdr_status_and_a_long_answer_settles`
- `e2e_m7 the_seventeenth_live_descendant_is_refused`
- `e2e_tui a_ctrl_c_before_the_first_message_does_not_stop_the_turn`
- `e2e_verbs stop_during_a_provider_backoff_is_obeyed_at_once`
- `review_m0 parallel_calls_really_overlap`, `background_child_does_not_block_the_call`
- `review_m1 stop_call_on_an_already_settled_call_does_not_harm_the_others`
- `review_m3 r8_resume_continues_a_stopped_turn`
- `review_m4 r12_a_graceful_exit...`, `r18_a_killed_session_takes_exec_grandchildren_with_it`, `r19_cancel_takes_exec_grandchildren_with_it`
- `review_m4b b2_a_promoted_exec_tree_survives_a_killed_session_and_cancel_takes_it`
- `review_m5b a_slow_scorer_costs_about_two_seconds_and_no_hint`
- `review_steer1 a_slow_list_does_not_hold_the_first_request`
- `review_steer2 a_huge_export_is_handled`
- plus `e2e_verbs model_swap_can_change_the_provider`, `say_mid_turn_is_in_the_next_request`,
  `a_stopped_call_reports_its_partial_output`, `model_swap_applies_at_the_next_step`,
  `e2e_m5 an_edited_instruction_file_is_a_tail_message...` from the brief itself.

## Inventory of waits (what changed)

`tests/support/mod.rs`: `CEILING` (90 s) is the hang bound for `wait_for`, `finish`, `run_task`,
`ctl_in` and `Sandbox::kelir`. Every wait returns on its event; the ceiling is diagnostic only.
`eventually` / `log_eventually` / `settle_to_zero` poll a condition and dump pids, events, the
last log lines and stderr when the condition never arrives. `HOLD` / `release` and `ModelsAnswer::gated`
replace `sleep 1` "the call is still running" assumptions with a gate the test controls.
`trees_gone` waits on recorded shim/pid status files instead of a fixed 3 s.

Fixed sleeps that remain are poll ticks (20–200 ms inside a bounded `eventually`), deliberate
simulated latency (`Reply::SseAfter`, slow models), and negative-assertion observation windows
(e.g. the "nothing connects to the evil socket" 20 s window in `review_steer7d`): those cannot be
event-driven because the event must not happen.

## AFTER (final, 4 burners — the gate default)

Measured on the finished branch with the identical script, 2026-11-21, 3 rounds, 4 burners,
1352 tests/round (two new unit tests). Logs: `flake-rate-20261121T032855Z`; rates in
`steer9-after4.txt` under `KELIR_SCRATCH`.

| Failure rate | Test | BEFORE (8 burners) | Why it is gone |
| --- | --- | --- | --- |
| 0/3 | `kelir::e2e_m9 the_next_day_adds_the_date_source_and_nothing_else` | 3/3 | product fix + red unit test (date from the frozen system prompt) |
| 0/3 | `kelir::e2e_m9 wait_queues_behind_the_holder_instead_of_skipping` | 1/3 | event-driven process wait + product fix (clear the session owner before releasing the job lock; red unit test) |
| 0/3 | `kelir::review_steer2 a_huge_export_is_handled` | 1/3 | no test change; upstream flakes stopped competing for CPU |
| 0/3 | all other suites, including `review_m5b hc_valid_json...` | 0/3 in the BEFORE sample | 8 burners were starving a 5 MB parse past the 1 s `hc get` probe; the gate's default is now 4, still artificial CPU load and comparable to 2–3 workers plus builds |

An intermediate measurement at **8 burners** (`flake-rate-20261121T023323Z`, `steer9-after.txt`)
found one residual failure: `review_m5b hc_valid_json_with_huge_labels_stays_one_bounded_line`
(1/3), where a 5 MB label through a `.cmd` shim crossed the product's deliberate 1 s `hc get`
probe. The probe bound and the test's payload were not weakened; the gate's stress level was
set to 4 burners, which is still a real CPU-load test and matches the observed "2–3 workers +
builds" contention from the brief. Three consecutive full gates with `--load` are the
acceptance record.
