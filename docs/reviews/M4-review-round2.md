# M4 review, round 2 — the fix round

Reviewer: Claude Code (reviewer/tester). Range reviewed: `5af7b0e..5779d3d` (the four `M4:` fix commits),
then re-run at HEAD (`0f1bbba`, which adds only the default-model pin and is unrelated to M4's behaviour).
Date: 2026-11-19. Host: a Windows 11 box (build 26300) for everything; Linux gate on a Linux box.

## Verdict: **PASS WITH FIXES** (0 blockers, 4 should-fix, 3 nits)

Both round-1 blockers are fixed and verified live: `bg cancel` and a killed session now take exec'd
grandchildren (`sleep.exe`, a nested `bash.exe`) down on Windows, and `--cwd` is honoured again. The four
round-1 should-fix items are fixed and each has a passing test. Every one of my 19 round-1 tests passes on
Windows and Linux. What is left is four things the fixes did not cover or introduced the edges of (G1-G4):
a pid-identity window that is still wide enough for Windows pid reuse, a *lost* promoted job that cannot be
cancelled, a Windows console-allocation stall that comes from starting every `bg run` shim `DETACHED`, and a
Unix linger loop that costs ~23 % of a core. None of them loses data or breaks the live scenarios.

## Gates (run by me)

| gate | result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --all-targets -- -D warnings` | clean (with my new test files) |
| `cargo test` (Windows, first cold run) | unit 257; e2e 16; e2e_m3 11; **e2e_m4 5 pass / 3 fail** (see "Environment stalls"); the rest not reached (cargo stops at the first failing binary) |
| `cargo test` (Windows, warm, `5779d3d` + my test fixes) | 257 unit; e2e 16; e2e_m3 11; e2e_m4 8; e2e_tui 15; e2e_verbs 22; review_m0 8; review_m1 5; review_m2 2; review_m3 11; review_m4 19 — all pass |
| stability, 5 consecutive runs, group A (`--lib` + e2e, e2e_m3, e2e_tui, e2e_verbs, review_m0..m3) | **347 passed, 0 failed, 5 of 5**; no stray `kelir`/`bash`/`sleep` afterwards |
| stability, 5 consecutive runs, group B (`e2e_m4` + `review_m4`) | **27 passed, 0 failed, 5 of 5**; no stray processes afterwards |
| `bash scripts/remote-check.sh <machine>` at `1b28ed3` and `09fb636` | fmt OK, clippy OK; 262 unit; e2e 16; e2e_m3 13; e2e_m4 8; e2e_tui 15; e2e_verbs 22; review_m0 8; review_m1 5; review_m2 2; review_m3 11; review_m4 19 — all pass (plus my `review_m4b`: 5 pass, 2 red by design, see G1/G2) |
| `cargo build --release` | ok, `kelir.exe` 6.19 MB (budget 20 MB) |
| default model | `0f1bbba` pins the default to `deepseek/deepseek-v4.1-flash-1017`. Nothing I ran depends on it: live runs pass `--model` explicitly, the e2e tests use `test/model`. |

The final Linux run at HEAD is recorded at the end of this file.

## Round-1 findings, re-checked

| round-1 | status | evidence |
|---|---|---|
| F1 blocker, exec'd grandchildren escape | **fixed** | live S3 (3 `sleep.exe` before cancel, 0 after), live S4 (tree gone 0.7 s after the kill), R18, R19, my B2 (promoted exec tree survives a killed session whole, then `cancel` takes all of it) |
| F2 blocker, `--cwd` ignored | **fixed** | R1; my B1 (cwd with a space and a non-ASCII letter reaches both the session's shim and the `bg run`-in-a-call route) |
| F3 argv boundaries | **fixed** | R2, unit test `bg_run_keeps_the_boundaries_the_caller_wrote` |
| F4 dead shim leaves `running` | **fixed** | R3; e2e `a_job_whose_shim_is_killed_settles_as_lost_and_wakes_the_model`; my B3 (an unpromoted *call* whose shim is killed settles `unknown`, its tree goes). See G2 for the part that is still open. |
| F5 cancel kills a recycled pid | **mostly fixed** | R5 (stale by months) passes; a pid reused inside the 120 s tolerance is still killed, see G1 |
| F6 leftover `&` child survives a killed session | **fixed**, documented | R11; my B5 (graceful exit also takes the leftover child); DEVIATIONS now says what is true on each platform |
| N1 `--follow` quadratic | fixed | R7 |
| N4 late promote ack | fixed (marker removed, last look at the edge) | code read |
| N5 wake between batch results | fixed (deferred after the batch) | R13 |
| N6 headless promoted text | fixed | unit test |
| N2 per-call overhead, N3 ambiguous ids, N7 self-declared `from: "job"` | unchanged, still nits | |

Regression hunt (task 3), all held: a promoted job survives a killed session with its whole exec tree (live S2,
B2); `kelir run --json | jq` does not hang, for `cmd &`, a promoted job, and `kelir bg run` made inside a call
(live S5, R14, B6: a 14 s job does not hold the pipe); cwd and argv quoting survive both start-up routes (B1,
R9); a call whose shim dies settles `unknown` and stops its tree (B3); a dead job shows `lost` and wakes the
model (e2e); torn / garbage status files are survivable (R15, R16).

## Findings

| # | sev | where | finding |
|---|---|---|---|
| G1 | should-fix | `src/bg.rs:53` `START_TOLERANCE = 120 s`, `shim_alive` ~line 190 | **The pid-identity check accepts a process that started up to two minutes after the job did.** `shim_alive` compares the status's `started` with the process's start time and calls them the same process within ±120 s. Windows reuses pids within seconds, so a shim that dies at 45 s whose pid is taken by an unrelated process at once is "alive" and **`bg cancel` terminates it**. Test B4: a `ping` started now, status `started` = now − 45 s, `bg cancel` kills it. Red on Windows and Linux. The tolerance exists for a clock that steps; a tighter, clock-independent check is available: have the shim read **its own** start time from the OS at launch (`GetProcessTimes` / `/proc/<pid>/stat`) and write it into the status (`shim_started`); the reader compares that exact value with `proc::start_time(shim)` (same clock, ±1 s). |
| G2 | should-fix | `src/bg.rs` `cancel` (line ~1280), DEVIATIONS "A job whose shim is gone…" | **A *lost* promoted job cannot be cancelled; its tree is orphaned.** DEVIATIONS says cancel on a lost job "on Windows still takes the tree down by its job-object name". A named Job Object only keeps its name while a handle is open; a promotion closes the session's handle and a killed shim closes the last one, so `OpenJobObject` finds nothing and `cancel` prints `lost (nothing to cancel)` while `bash.exe` and both `sleep.exe` keep running. On Unix `cancel` refuses to signal by pid, so the same happens. Test B7 (session killed, then the shim killed, then `bg wait` -> `lost`, `bg cancel`): 5 processes left on Windows, same on Linux. The likely fix is the G1 token: once identity can be proven, `cancel` can take the **command's** tree by its recorded pid/pgid (Unix `killpg` of the shim's group while any member's start time matches, Windows tree walk from `pid`). Until then the DEVIATIONS sentence is wrong. |
| G3 | should-fix | `src/bg.rs:404` and `:520` (`DETACHED_PROCESS` for the `bg run` shim) | **Every `bg run` job on Windows pays for a fresh console, and under concurrency that stalls for seconds to minutes.** The shim is `DETACHED_PROCESS` (no console), so its console-subsystem child `bash.exe` allocates a new console, i.e. a new `conhost.exe`, per job. Measured at OS level with a small Python harness that mimics the shim (40 concurrent `bash.exe -c 'echo hi'` under a parent started with each flag): **`DETACHED_PROCESS` median 7.98 s / max 11.2 s (second round 4.26 s / 8.1 s); `CREATE_NO_WINDOW` median 0.10 s / max 0.25 s; inherited console 0.12 s / 0.20 s.** One job: 0.33 s vs 0.04 s; eight: 1.24 s vs 0.09 s. In the test suite this is what stalls: during degraded episodes I found 100-220 shims at once, each with a `bash.exe` stub waiting at 0 CPU with a `conhost.exe` child (some for more than 3 minutes), and `e2e_m4` `cancel_kills_a_tree…`, `a_bg_run_inside_a_call…`, `a_job_started_inside_a_call…` plus `review_m4` R2/R4/R6/R7/R8/R16 and, in the cascade, even unrelated timing tests (`stop_ends_the_turn`, `parallel_calls_really_overlap`) failed. Calls are not affected (their shim inherits the session's console). Suggested fix: spawn the `bg run` shim `CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP` instead of `DETACHED_PROCESS` (still outside the session's console and, with `CREATE_BREAKAWAY_FROM_JOB`, outside its job); I could not try it in kelir itself (no `src/` changes in this role), only on the OS primitive. Also a visible console window per job on a real desktop session is plausible with `DETACHED_PROCESS`; I did not look for it. |
| G4 | should-fix | `src/bg.rs:818` `linger`, `src/proc.rs` `group_others` | **A lingering Unix shim costs a quarter of a core.** While a finished call's `cmd &` child lives, the shim scans `/proc` every `POLL` = 25 ms (one `stat` read per process). Measured on a Linux box with the release build (548 processes): **230 CPU ticks in 10 s = 23 % of one core per lingering shim**, for as long as the child lives (a dev server started with `&` = hours). Each call that leaves a child adds one. The owner check is cheap (`getppid`) and can stay at 25 ms; the group count only needs to run about once a second, or use a cheaper signal. |
| N8 | nit | `tests/e2e_m4.rs` (`cancel_kills_a_tree_of_children…`) | The test starts an infinite `while true` job and only cancels it at the end; when it times out earlier (G3) the loop and its shim are leaked for good. My first run left four of them until I killed them by hand. A drop guard that cancels the job would keep a failure from poisoning the box. |
| N9 | nit | `src/bg.rs` `spawn_shim_for_client` | `bg-spawn` lets any client of the session socket with `from: "job"` (self-declared) make the session start an arbitrary shim command in an arbitrary cwd. Same user, same trust boundary as `exec` and the Windows pipe's default ACL keeps others to read, so low risk; mention it next to round 1's N7. |
| N10 | nit | `docs/DEVIATIONS.md` | The sentence on a lost job's cancel (G2) and the 120 s tolerance wording ("compared with `started`, within 120 s") should change with G1/G2. |

## The known flake: `a_crash_at_any_of_five_points_resumes_without_re_running_a_command`

**Verdict: a test race exposed by the M4 design, not a product bug.** (And it was not alone: `review_m3`
R4 has the identical assumption and failed the same way in my runs.)

- The `mid_tool` hook aborts the session 150 ms after dispatching the call. Before M4 the command was one
  `bash` spawn and had written by then. Now a call is `kelir bg-shim` -> `bash` -> `printf`, and by design an
  unpromoted tree dies with the session (kill-on-close / the shim's owner watch). A loaded box can therefore
  abort before the command's first write; the call's outcome really is unknown (the log says so), so 0 runs
  is as honest as 1.
- Reproduced: 16 busy loops on this 12-thread box -> the old test failed **10 of 15** runs with exactly
  `mid_tool: the command ran 0 time(s), expected 1`; idle it passed 15 of 15.
- Fix (`1b28ed3`, test only): take the run count once the trees are gone (750 ms), then assert the real
  invariants: **a resume never changes the count** (a completed command is never re-run, and a resume never
  adds a run of its own), `mid_tool <= 1`, `tool_settled == 1`, every other point `== 0`, and the log has no
  second `tool_started` (and none at all for `usage` / `mid_stream`, which die before dispatch). The other
  four crash points keep their exact expectations. Under the same 16-loop load the new test passes **15 of 15**.
  My first draft of the `tool_started`-count assertion was wrong for the `usage` point (the call was never
  started); that is corrected in the commit.
- `review_m3` R4 (`587c640`): same race (failed 2 of 10 alone on a loaded box), same fix; 15 of 15 under load.
- Stability: the nine non-`bg run` binaries pass 5 of 5 consecutive runs (347 tests each).

## Environment stalls (read before judging a red `cargo test` on this box)

The `bg run` suites (`e2e_m4`, `review_m4`) passed 5 of 5 times in a calm state but failed in several
degraded episodes (the very first cold run; runs 2-5 of a whole-suite loop; later runs of a second loop). In
each episode the box was holding 100-220 stuck `bg-shim`/`bash.exe`/`conhost.exe` stubs and a cleanup of ~450
processes was needed. I traced it to G3 (console allocation for a detached shim); the OS-level measurement
reproduces the stall without kelir. My own `review_m4b` tests run many PowerShell/WMI queries in parallel and
probably pushed it over the edge in two episodes, so they were the likelier trigger there. After killing the
strays by pid (never `taskkill /IM`; other agents' kelir sessions run on this box) the suites were green again.
**No stray processes remain from the stable runs** (before/after `tasklist` equal for kelir, bash, sleep).

## Live drive (Herdr panes, `deepseek/deepseek-v4.1-flash-1017`, `scripts/live/m4_*.sh`, root `kelir-scratch/M4b`)

Started with `hc run --bg --name live-m4b-sN --cwd <scratch> "C:/Program Files/Git/bin/bash.exe" -c 'set -a; . live.env; set +a; bash scripts/live/m4_<scenario>.sh <scratch> | tee pane-sN.txt'`.
OS process lists were taken before and after by the scripts (`Get-CimInstance Win32_Process`).

| # | scenario | what I saw | verdict |
|---|---|---|---|
| S1 | model runs `kelir bg run "sleep 60; echo ok"`, carries on, is woken | `bg_started` at +2 s (call_id = the bash call), turn ended `waiting`, `bg_settled {exited, 0}` 59 s later then `user {from: job}` `[bg job bg_… exited 0 after 60.5s]`, model ran `kelir bg out` and answered `ok`; no `sleep 60` left | **PASS** |
| S2 | `KELIR_PROMOTE_AFTER=5`, 20 s loop, `taskkill /F /PID <session>` | `tool_settled outcome=promoted` at +6 s; after the kill command pid 46892 and shim 12208 ALIVE; `bg out` (no session) `tick 1 tick 2`; `bg wait` -> `6-1-1 exited exit 0 after 20.3s`; `resume` logged `resumed`, `bg_settled {exited, 0}`, `user from job`, `session_ended` | **PASS** |
| S3 | job with a child of a child (`bash -c 'sleep 301 & sleep 302 & wait' & sleep 303 & wait`), `bg cancel` twice | before: shim, 3 `bash.exe`, **3 `sleep.exe`** (301/302/303); cancel -> `cancelled`; again -> `cancelled (nothing to cancel)`; after 2 s: **0** left; session logged `bg_settled {state: cancelled}` | **PASS** (round 1: FAIL) |
| S4 | unpromoted `bash -c "sleep 311 & sleep 312 & wait"`, session killed | before: shim, 3 `bash.exe`, 2 `sleep.exe`; "tree gone 0.7 s after the kill"; none left | **PASS** (round 1: FAIL) |
| S5 | `kelir run --json \| jq`, with `sleep 25 &` and with a promoted `sleep 25` | pipelines finished in 2.4 s and 6.5 s; the 25 s job kept running detached, no pipe held | **PASS** |

Go provider: not live-verified (no key), not touched by M4.

## Tests added or changed by me

- `tests/e2e_m3.rs`, `tests/review_m3.rs`: crash-timing races fixed as described above (commits `1b28ed3`, `587c640`).
- `tests/review_m4b.rs` (commit `09fb636`, run `cargo test --test review_m4b`): B1 cwd with a space / non-ASCII on both start-up routes (pass) · B2 promoted exec tree survives a killed session and `cancel` takes it (pass) · B3 unpromoted call whose shim is killed settles `unknown`, tree goes (pass) · **B4 pid recycled within the tolerance (G1, red on Windows and Linux)** · B5 graceful exit takes a leftover `&` child (pass) · B6 `bg run` in a call and a promoted call do not hold the pipe (pass) · **B7 a lost promoted job can still be cancelled (G2, red on Windows and Linux)**. The two red tests are the proof of G1/G2 and stay red until the fixes. B-markers use fractional sleeps (`sleep 7411.11`) because a bare four-digit marker matches random temp-dir names.

## Spec / task items not yet covered

- "cancel kills the job's process tree" holds for a live job; it does not for a lost one (G2).
- The default 600 s promotion path is still only unit-tested (the live drive uses 5 s).
- macOS only compiles; the Unix linger needs `/proc` and does not linger there (documented).
- A visible console window per `bg run` job on a real Windows desktop was not checked (G3).

## Suggested order of fixes

1. G3 (`CREATE_NO_WINDOW` on the `bg run` shim): it is what makes the Windows suite flaky and every job slower.
2. G1 + G2 together (the shim records its own start time; cancel uses it to reach a lost job's tree).
3. G4 (poll the group count at ~1 s).
4. N8, N10. Then rerun `review_m4b` (B4 and B7 must go green) and the live S3/S4.

## Final Linux gate at HEAD `0f1bbba` (a Linux box)

fmt OK, clippy OK; 262 unit; e2e 16; e2e_m3 13; e2e_m4 8; e2e_tui 15; e2e_verbs 22; review_m0 8; review_m1 5;
review_m2 2; review_m3 11; review_m4 19 — all pass. `review_m4b`: 5 pass, B4 and B7 red (G1, G2), as on Windows.
