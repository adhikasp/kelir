# M4 review — background jobs

Reviewer: Claude Code (reviewer/tester). Range reviewed: `99b3040..7da31b9` (the four `M4:` commits).
Date: 2026-11-19. Host: the Windows box (Windows 11) for everything; Linux gate on a Linux box.

## Verdict: **FAIL** (2 blockers, 4 should-fix, 7 nits)

The core feature works and survives the live drive it was built for: a real model starts a `kelir bg run`
job, carries on and is woken; a call promoted at 5 s survives `taskkill` of the session, `bg out` and
`bg wait` read it with no session, `resume` folds it in as `bg_settled` plus the wake message;
`kelir run --json | jq` does not hang on `cmd &` or on a promoted job. The **tree-kill half does
not work on Windows with Git's msys bash**: `bg cancel`, and the "session died, take the tree with it"
guarantee, leave exec'd grandchildren (`sleep.exe`, a nested `bash.exe`) running. That is the milestone's
item 5 ("cancel kills the job's process tree") and the "no orphan" requirement, and it is a
**regression against M3** (the same scenario passes at `99b3040`). A second regression: `--cwd` is
no longer honoured by `bash` calls.

## Gates (run by me)

| gate | result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --all-targets -- -D warnings` | clean (also with `tests/review_m4.rs`) |
| `cargo test` (Windows), implementer's suites | 254 unit; e2e 16, e2e_m3 11, **e2e_m4 6**, e2e_tui 15, e2e_verbs 22, review_m0 8, review_m1 5, review_m2 2, review_m3 11 — all pass |
| `bash scripts/remote-check.sh <machine>` (Linux, HEAD `7da31b9`) | fmt OK, clippy OK; 259 unit; e2e 16, e2e_m3 13, e2e_m4 6, e2e_tui 15, e2e_verbs 22, review_m0 8, review_m1 5, review_m2 2, review_m3 11 — all pass, exit 0 |
| `cargo build --release` | ok, `kelir.exe` 6.13 MB (budget 20 MB) |
| `tests/review_m4.rs` (mine, 19 tests) | Windows: 12 pass, **7 fail on real bugs** (R1, R2, R3, R5, R11, R18, R19); Linux: 14 pass, **5 fail** (R1, R2, R3, R5, R11) |

Idle RSS was not re-measured (no change to the idle path; the session adds one 250 ms watcher task).

## Findings

| # | sev | where | finding |
|---|---|---|---|
| F1 | **blocker** | `src/proc.rs:348` (`LimitFlags = JOB_OBJECT_LIMIT_BREAKAWAY_OK`), `src/bg.rs:317-` | **Exec'd grandchildren escape the call's Job Object on Windows**, so neither `bg cancel` nor "the session died" kills them. Cygwin/msys `spawn` asks for `CREATE_BREAKAWAY_FROM_JOB` whenever the job allows it, and M4 added `BREAKAWAY_OK` ("so a `kelir bg run` inside a call is never trapped"). Scenario: `kelir bg run "bash -c 'sleep 301 & sleep 302 & wait' & sleep 303 & wait"`, then `kelir bg cancel <id>` -> status says `cancelled`, but `sleep.exe 301/302/303` and the inner `bash.exe` keep running with dead parents (live S3: 3 `sleep.exe` before, 3 after). Same for an unpromoted call whose session is killed (live S4: `sleep 311/312` + inner bash survive; "tree gone" never happens). Tests R18, R19 fail at HEAD, **R18 passes at `99b3040`**. Verified cause: a scratch copy of HEAD with only `LimitFlags = 0` passes R18 and R19. The implementer's own tree test passes because it uses fork-only subshell loops, not exec'd children. Trade-off to settle: a `kelir bg run` made inside a call then sits in the call's job (its shim already retries without breakaway) — give those jobs their own handling instead of a job-wide breakaway. |
| F2 | **blocker** | `src/bg.rs` `shim_command`, `src/tools/bash.rs` (no `current_dir`) | **`--cwd` (and a resumed session's recorded cwd) is ignored by `bash`.** M0-M3 set `current_dir(&ctx.cwd)` on the shell; M4 starts the shim with no `current_dir`, so the command runs in the *process's* directory. `kelir run --cwd <other> "pwd > where.txt"` writes `where.txt` into the launch directory (R1; passes at `99b3040`, fails at HEAD). The `edit` tool still resolves against `ctx.cwd`, so a session now edits in one directory and runs commands in another. |
| F3 | should-fix | `src/bg.rs` `run()` line 701 | `kelir bg run` joins its argv with spaces, so argument boundaries are lost: `kelir bg run printf "[%s]" "a b" c` runs `printf [%s] a b c` (output `[a][b][c]`, R2); `kelir bg run bash -c 'echo one; echo two'` prints a blank line and `two`; `kelir bg run echo "a  b" "c'd"` is a shell syntax error. Documented in `--help`, but it is the opposite of how every model writes a command. Quote each word when there is more than one. |
| F4 | should-fix | `src/bg.rs` shim loop / `watch_bg` / `resume` | **A dead shim leaves `state: running` for ever.** Kill the shim from outside (`taskkill /F /PID`, an OOM kill, a reboot — note `taskkill /IM kelir.exe` does exactly this): `bg wait` waits for ever (R3), `bg ls` reports `running`, the session's watcher never wakes the model and `resume` never settles the job. Nothing checks that `shim` is alive. |
| F5 | should-fix | `src/bg.rs` `cancel` + `kill_tree` | `cancel` trusts the `shim`/`pid` in the status file. With a stale status (F4, or a reboot) whose pid has been recycled, `bg cancel` terminates an unrelated process (Windows `TerminateProcess`; Unix `killpg`/`kill`). R5 writes a status naming a live `ping` and `bg cancel` kills it. Check liveness plus identity (process start time, or the image is kelir) before signalling, and mark a dead job `lost`. |
| F6 | should-fix | `src/proc.rs` (job no longer kill-on-close), DEVIATIONS "Windows" para | A child left by a **finished** call (`cmd &`, a dev server) used to die with the session on Windows even when the session was killed (kill-on-close). Now it survives a killed session (the shim has already exited, nothing watches the owner), and it survives on Unix too (killpg only runs on a graceful exit). R11: a `for ... echo tick >> left.txt &` keeps writing after the session is killed; **passes at `99b3040`, fails at HEAD.** DEVIATIONS says "it dies when the session does", which is only true for graceful exit. Restore kill-on-close for unpromoted calls (set it on the job and clear it on promotion), or document and test the weaker guarantee. |
| N1 | nit | `src/bg.rs` `follow` | `bg out --follow` re-reads the whole file every 25 ms. Fine on the 120 MB finished-job case (R7 passes, under 10 s), O(n^2) while a big job is still running. |
| N2 | nit | `src/tools/bash.rs` | Per-call overhead: every call now starts a second `kelir` process and polls at 25 ms. 20 trivial calls: mean `tool_started`->`tool_settled` **98 ms at `99b3040`, 174 ms at HEAD** (R17; machine was busy with other panes, so treat as +50..80 ms). |
| N3 | nit | `src/bg.rs` `find` | A promoted job's id is `<seq>-<step>-<n>`, so the same id exists in many sessions. From a bare shell `kelir bg out 6-1-1` returns the newest session's job, `cancel` would cancel it. Inside a session `KELIR_SESSION_DIR` makes it unambiguous. |
| N4 | nit | `src/tools/bash.rs`, `PROMOTE_ACK` | If the shim does not acknowledge a promotion within 5 s the call carries on unpromoted, but the shim may still see the marker later and set `promoted: true` (no more owner watch, and the watcher records a bogus `bg_started`/wake for a call whose result the model already got). |
| N5 | nit | `src/session.rs` ~2330 | `note_bg` after a promoted `tool_settled` can write the wake `user` message between the `tool` results of one batch if the job settles before the batch's results are written; the fold does not reorder. R13 tried 10 durations around the promotion edge and never hit it, so it is theoretical; worth a guard (defer the wake to the boundary). |
| N6 | nit | `docs/DEVIATIONS.md` | A non-`--keep` `kelir run` ends after the turn; the promoted result tells the model "you will get a message when it settles" but nothing delivers it until a resume. Say so in the result text for headless runs. |
| N7 | nit | shim env | `KELIR_BIN` is exported but `kelir` itself is only found if it is on `PATH`; the base prompt (M5) must say how a call reaches it. Also the `from: "job"` that makes the `bg` verb internal is self-declared on the socket, so "a client cannot fabricate a job" holds for `kelir ctl` only. |

Observed, not an M4 defect: msys `bash.exe -c` halves doubled backslashes inside single quotes
(`'a\\b'` prints `a\b`), identical when called directly from Python; same at M0.

Things I tried to break that held: torn/empty/binary/wrong-type/`..` status files and ids (R15 and unit
tests); a status torn and then deleted while its job runs, the shim rewrites it and the wake still arrives
(R16); two cancels, cancel after finish, unknown id (R4); 100 concurrent quick jobs: unique ids, all
`exited 0`, no `.tmp` leftovers (R6); a 120 MB job (R7); an invalid command (exit 127) and a missing shell
(R8, the call result says `the command did not run`); quoting battery through two Windows command lines
(R9: embedded quotes, trailing backslash, UTF-8, heredoc, `%PATH% ^& | > <`, `$((...))`); a 32,000-char
command still runs (R10); `run --json` does not hold the pipe for `sleep 12 &` (R14); `ctl end` leaves a
promoted job running (R12).

## Live drive (Herdr panes, `deepseek/deepseek-v4.1-flash-1017`, scripts in `scripts/live/m4_*.sh`)

All run with `hc run --bg --name live-m4-sN ... bash -c 'set -a; . live.env; ...'`; transcripts are in the
scratch dir `kelir-scratch/M4/pane-*.txt` (not in the repo).

| # | scenario | what I saw | verdict |
|---|---|---|---|
| S1 | model starts `kelir bg run "sleep 60; echo ok"`, carries on, is woken (`--keep`) | `bg_started` (call_id = the bash call) at +2 s; model ran `echo working > progress.txt`, answered `waiting`; `bg_settled {state: exited, exit_code: 0}` 60 s later, then `user {from: "job"}` `[bg job bg_01m7rb1d... exited 0 after 60.3s]`, the model ran `kelir bg out <id>` and answered `ok`; no `sleep 60` left | **PASS** |
| S2 | `KELIR_PROMOTE_AFTER=5`, a 20 s loop, `taskkill /F /PID <session>`, job survives, `bg out`, `resume` | `tool_settled outcome=promoted` at +6 s (result names `bg job 6-1-1`, `kelir bg out/wait/cancel`); after the kill command pid 19572 and shim 34040 both ALIVE; `bg out` (no session) printed `tick 1 tick 2`; `bg wait` -> `exited exit 0 after 20.2s`; `resume` logged `resumed`, `bg_settled {exited, 0}` and `user from job` and ended `session_ended` | **PASS** |
| S3 | job whose command spawns a child of a child, then `bg cancel` (twice) | before: 3 `sleep.exe` + 3 `bash.exe` + shim; `cancel` -> `cancelled`, second -> `(nothing to cancel)`, session logged `bg_settled {cancelled}`; **after: `bash.exe -c "sleep 301 & sleep 302 & wait"` and `sleep.exe` 301, 302, 303 still running** (rerun with a corrected process filter: `count before=3 after=3`) | **FAIL (F1)** |
| S4 | unpromoted call `bash -c "sleep 311 & sleep 312 & wait"`, session killed | the shim/bash wrapper chain died, but **`sleep.exe` 311/312 and the inner `bash.exe` survived** (`before=2 after=2`) | **FAIL (F1)** |
| S5 | `kelir run --json \| jq` with `sleep 25 & echo started`, and with a promoted `sleep 25` (promote_after=4) | pipelines finished in about 1.9 s and 6.0 s (log timestamps); the 25 s job kept running detached, no pipe held | **PASS** |

I killed the leftover test processes by pid afterwards. No `taskkill /IM` was used (other kelir sessions run on this box).

Go provider: not live-verified (no key); not touched by M4.

## Tests added (all in `tests/review_m4.rs`, run `cargo test --test review_m4`)

R1 `--cwd` (F2, fails) · R2 argv boundaries (F3, fails) · R3 dead shim (F4, fails) · R4 cancel twice/after
finish/unknown id · R5 recycled pid (F5, fails) · R6 100 quick jobs · R7 120 MB job · R8 invalid command and
missing shell · R9 quoting battery · R10 32 kB command · R11 leftover `&` child after a killed session (F6,
fails) · R12 graceful exit leaves a promoted job · R13 wake never between batch results · R14 `--json` pipe ·
R15 garbage in `bg/` · R16 torn/deleted status mid-job · R17 call latency (prints, asserts < 2 s) ·
R18 exec'd grandchildren die with a killed session (F1, fails) · R19 `cancel` takes exec'd grandchildren (F1, fails).
Seven fail at HEAD on Windows; R1, R11 and R18 pass at `99b3040`. **On Linux (a Linux box)** 14 pass and 5 fail: R1, R2, R3, R5, R11 fail there too (F2, F3, F4, F5, F6 are not Windows-only), while R18/R19 pass (F1 is Windows-only: Unix `killpg` takes the whole group).

## Spec / task items not yet covered

- "cancel kills the job's process tree" and "no orphan" — not true on Windows with msys (F1).
- Spec *Process model*: a killed session takes its calls' trees with it — only for the call still running (F1 aside); a call that already returned leaves its `&` child (F6).
- `kelir bg ls` shows one directory (documented gap); nothing sweeps `bg/` (documented).
- The 15-minute figure of the spec table was exercised as 5 s / 20 s (as the task allows); the default 600 s path is unit-tested only.
- macOS only compiles.

## Suggested order of fixes

1. F1: drop `JOB_OBJECT_LIMIT_BREAKAWAY_OK` from the call job (verified fix); decide how a `bg run` inside a call detaches.
2. F2: `current_dir(ctx.cwd)` on the shim command (the shim passes it on by inheritance).
3. F6: kill-on-close on the unpromoted call's job (clear it on promotion).
4. F3, F4+F5 (liveness check of the shim pid before reading `running` / before signalling).
Then rerun R1-R19 and the live S3/S4.
