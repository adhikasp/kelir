# M9 fix review — verdict PASS

Reviewer: fresh QA (kelir session, opencode-go/hy4-flash-free). Worktree
`C:/Users/the maintainer/kelir-wt/m9fixrev`, branch `job/m9fixrev` (based on `job/m9fix`
at `4131191`). Diff reviewed: `77f5568..4131191`, plus my own test commit
`dfd131c` (only `tests/review_m9fix.rs`). No `src/` was modified by me.

## Verdict

**PASS — 0 blockers, 0 should-fix, 0 nits remaining.** Every finding in the
original `docs/reviews/M9-review.md` is closed, and the fixer's claim that the
original B1 diagnosis was incomplete is **confirmed**: `def.max_steps` *was*
already passed through `CommonArgs` to `prepare()` (`src/jobrun.rs:330` old →
`src/session.rs:1366-1368`), so the fix is not a plumbing change but a new
job-specific pause rule (`src/jobs.rs:46-50`, `src/session.rs:5097-5106`) that
overrides STEER-4's "answer on the cap-th step is `done`" for scheduled runs.
`--max-steps 0` lifts the cap. The original failure was exactly the reported
`left: "done", right: "paused"`; I verified the pre-fix code path by reading
`git show 77f5568:src/jobrun.rs` and `git show 77f5568:src/session.rs`.

## What I verified, with proving commands

### B1 — job `max_steps` is a real step cap (closed)

| claim | proof |
|---|---|
| `tests/review_m9.rs::task_file_and_max_steps_are_honoured` now passes with `paused` | `cargo test --test review_m9 task_file_and_max_steps_are_honoured` → 1 passed (`tests/review_m9.rs:341` asserts `paused`) |
| e2e cap test | `cargo test --test e2e_m9 a_jobs_max_steps_pauses_the_run_at_the_cap` → 1 passed (STATUS `paused`, `steps=1`, exactly one request, `tests/e2e_m9.rs:402-421`) |
| `--max-steps 0` lifts the job cap | `cargo test --test e2e_m9 a_zero_max_steps_flag_lifts_a_jobs_cap` → 1 passed (`tests/e2e_m9.rs:425-439`) |
| full shipped suites | `cargo test --test review_m9` → 12 passed; `cargo test --test e2e_m9` → 20 passed |

My own adversarial probes (`cargo test --test review_m9fix` → 6 passed, all
spawn-capped and Drop-guarded):

- `a_job_cap_th_answer_after_a_tool_pauses_after_the_tool_result`: a job with
  `max_steps = 2` whose cap-th step contains a tool call **and** the final answer
  pauses after the tool result (`turn_ended.reason = "steps"`, `final_text`
  kept, both tool calls settled before `turn_ended`, no third request). The
  new check sits after `after_step` (`src/session.rs:5080-5106`), so a late
  boundary cannot slip past it.
- `the_job_cap_is_not_in_the_model_prompt_and_is_not_misreported`: in the same
  sandbox, a capped run vs the same job run with `--max-steps 0` have identical
  frozen system prompts, a byte-identical prefix, no cap word anywhere in the
  request, and only STATUS/events differ (`paused` vs `done`, reason `steps`,
  no internal `job_step_cap` in STATUS or the log).
- `interactive_max_steps_keeps_the_cap_th_answer_done`: a plain
  `kelir run --json --max-steps 1` whose cap-th step is the answer still ends
  `done` (`tests/review_steer4.rs:47` and `tests/e2e_steer4.rs:66` also pass;
  the full STEER-4 suites were 9/9 and 6/6). The Plan is process-local
  (`src/jobs.rs:64-73`), so a job cannot leak its pause rule into interactive
  runs.

### S1 — rotating runs: two tail sources, prefix untouched (closed)

`docs/reference/jobs.md:155-158` now states that a rotating run adds a
**second** tail `carry` source before the `job` line, that both go after the
frozen prefix, and why they stay separate. Code: `src/jobrun.rs:258-292`
pushes `carry` then `job`; `src/session/jobhooks.rs:20-31` records them at the
tail. My `a_rotating_job_adds_carry_then_job_after_the_frozen_prefix` confirms
the new session's last three request messages are carry, job, task, and its
log has `source` kinds `["carry", "job"]` in that order. Same-session cache
prefix stability remains covered by `tests/e2e_m9.rs:160-166` and
`:511-513`.

### S2 — unreadable young lock: reason, no takeover (closed)

`src/joblock.rs:98-102` adds `Holder.reason`; `:258-270` fills it only when no
record is readable; `judge(None, young, _)` stays `Fresh` (`:80-87`), so a
fresh lock is never taken over. `src/jobrun.rs:145-163` prints the reason on
stderr and records it in the skipped history line. My
`an_unreadable_young_lock_is_respected_and_the_skip_explains_why` drives the
real binary: exit 75, `unreadable` in stderr and `runs.jsonl.error`,
`holder_pid: null`, the lock bytes unchanged, zero model requests, no key in
stderr. Pid-reuse safety: `cargo test --lib joblock` → 11 passed (including
`a_reused_pid_is_not_a_live_holder` and the new reason test); my
`an_old_lock_with_a_recycled_pid_is_taken_over` confirms a dead OLD lock whose
pid is now alive is still reclaimed (start-time check intact).

### n1 / n2 — documented accurately (closed)

`docs/reference/jobs.md:26` names the leading letter/digit rule (code:
`src/names.rs:22-50`); `:29` names the bind-before-start `new` state (code:
`src/names.rs:276-300`, `src/names.rs:430-440`). Both match the code.

### Deviation record

`docs/DEVIATIONS.md:1754-1759` accurately records the job-cap-vs-STEER-4
difference and the `CommonArgs` plumbing. The M9 review's "Fix status" section
(`docs/reviews/M9-review.md:7-25`) matches the code and my results.

## Gate (full, with Linux)

Proving command: `bash scripts/gate.sh --linux=the maintainer` at commit `dfd131c`
(machine name on the command line only). It waited ~10.5 min for another
worker's gate lock (correct: I did not bypass it). Logs:
`/c/Users/the maintainer/kelir-scratch/gate-job-m9fixrev-20261120T231554Z/`.

```
gate: dfd131c M9fix review: adversarial probes for cap, sources, lock and STEER-4
  fmt: ok (1997 ms)
  clippy: ok (1982 ms)
  nextest: ok (250343 ms)
  result: Summary [ 196.559s] 1355 tests run: 1355 passed, 3 skipped
  doctest: ok (2663 ms)
  linux (the maintainer): ok (220393 ms)
gate: PASS
```

Linux half: `FMT-OK`, `CLIPPY-OK`, `Summary [120.572s] 1356 tests run: 1356
passed, 3 skipped`, `NEXTEST-OK`, `DOCTEST-OK`, `remote-check: the maintainer rc=0`
(work dir `~/kelir-verify-m9fixrev/src-tree`). The extra test on Linux is my
new integration target's count; no retries, no flakes, no leaks were reported
(`grep -c 'RETRIED\|TRY 2\|FLAKY'` on the nextest log = 0). `cargo test --lib
docs` → 24 passed (no drift).

## Process and repo hygiene

Before and after the gate, `sh.exe` count was 1 (an unrelated long-running
shell, PID 62420, unchanged). At gate completion `kelir.exe` count was 0. A
later check found 4 `kelir.exe` processes whose command lines point at
`kelir-wt/m10rev/target/debug/kelir.exe` (another worker's `channel --server
fake` tests); I did not touch them — AGENTS.md forbids killing by image name
and touching other worktrees. None of my tests' processes leaked: each uses
the `Sandbox`/`TreeGuard` Job Object, a per-file spawn cap of 80, and a
`clean()` assertion that nothing is alive at the end; the nextest gate also
reported no leaks. No test signals a process group and no console window is
created (`CREATE_NO_WINDOW` in the harness). No keys, hostnames, usernames or private paths are in the diff
(`git diff 77f5568..HEAD | grep -inE 'sk-…|an address|100\.117\.193\.118|/Users/the maintainer|the maintainer'`
→ nothing).

## Gaps (carried, not introduced)

- Live 75/124 against a deliberately stalled real provider remains undriven
  (S3 from the original review); mock e2e with real processes proves both exit
  codes. No product change was needed for the fix pass, and the brief for this
  fix review did not ask for a new live drive. The original M9 live job
  resume/cache verdict (PASS) still stands.
- The fix commits did not touch behavior other than the job cap and the skip
  diagnostic; no regression found in the full 1355/1356-test suites.
