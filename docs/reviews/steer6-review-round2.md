# STEER-6 fix round — review round 2 (job steer6b)

Reviewer: fresh QA agent. Range `git diff 43a3ce5..HEAD` (fix commits `4e506a0..ed4c059`), branch `job/steer6b`,
worktree `kelir-wt/steer6`, own target dir. Windows 11 (Git for Windows bash) and Linux (the maintainer). Round 1: `docs/reviews/steer6-review.md` (FAIL).
(The task asked for both `steer6-review-round2.md` and `steer6b-review.md`; this file is the report, `steer6b-review.md` is a pointer.)

## Verdict: PASS WITH FIXES — 0 blockers, 2 should-fix, 4 nits

B1, S1, S2, S3, N1, N2, N3, N4 from round 1 are all fixed and re-verified on the real release binary. No regression found in exit codes,
cwd, env, stdin, unicode, CRLF, 100 KB commands, edit atomicity or the cache prefix. The two should-fix items are new and small: the always-file route
puts a prompt echo in every `cmd.exe` result (N6), and a shell argv whose last word is not `-c`/`/C`/`-Command` (e.g. `["bash","-lc"]`) silently keeps
the old halving route (N5).

## Gates

| gate | result |
|---|---|
| `cargo fmt --check` | clean (also with my test file) |
| `cargo clippy --all-targets -- -D warnings` | clean (with my test file) |
| `cargo test --no-fail-fast` (Windows, 1 full run) | every suite green (325 unit, e2e 16, e2e_m3 11, e2e_m4 10, ... 0 failures) |
| `cargo test --test review_steer6b` (mine) | 14 pass, 2 `#[ignore]`d prove N5/N6 |
| `scripts/remote-check.sh the maintainer kelir-verify-steer6b-rev2` (Linux, HEAD ed4c059) | fmt ok, clippy ok, 330 unit; first plain `cargo test` run stopped at `review_m4b` b2+b7 (promoted-tree / lost-job cancel). Both pass 2/2 in isolation and the whole `--no-fail-fast` re-run is green (all suites). Load flake, same family as the known m4 timing flakes; not counted against the branch. |

Note: my first two Linux runs overlapped in the same work dir (I started the script twice) and clobbered each other's `src-tree`; the figures above are from a clean third run.

## Round-1 findings, re-verified

| id | status | evidence |
|---|---|---|
| **B1** cmd.exe dropped `/C` | **fixed** | Real binary, pane `live-s6b-cmd`, `shell=["cmd.exe","/C"]`: short command runs (`short.txt`), a **20 KB** command (`rem xxx…` + CRLF + `echo`) runs (`long.txt`), `exit /b 7` is recorded `exit 7`. Test `b1_cmd_exe_short_long_exit_cwd` (also exit 5, cwd, `\\` and `%%` through the .bat). Round-1's `f2_cmd_exe_runs_a_long_command_from_its_bat_file` green. A state dir with a space + non-ASCII (`sp ace é`) works on bash and cmd.exe. PowerShell: pane `live-s6b-ps`, `-NoProfile -Command` becomes `-File cmd.ps1`; short, 20 KB and `exit 7` all correct; quotes/`$x`/é/`\\` survive (`n2_powershell_…`). |
| **S1** backslash halving | **fixed** | Real binary via `kelir bg run` in a pane (`steer6b_review_shell.sh`): runs of 1,2,3,4,5,7,8,9,16,100,1000,5999,6001,9000 backslashes through a quoted heredoc all byte-exact. Test `s1_backslash_runs_are_byte_exact_at_every_length`: 62 lengths (1..40, 63/64, 100, 255/256, 999/1000, 4095/4096, 5990..6100, 8000, 8185..8200, 9000) via the bash tool, byte-exact. `s1_printf_sed_regex_and_paths_mean_the_same_short_and_long`: printf `\\n`, `\t`, a `C:\Users\…` path, `sed 's/a\\/X/'`, `grep -E 'a\.b'`, `\"`, unquoted `\\\\` give identical bytes on the 100-byte and the 7 KB command and the expected shell meaning. Also green on Linux (same test, argument route). |
| **S2** parallel_tool_calls vs slow list | **fixed** | Live, pane `live-s6b-s2`: **clean state dir**, proxy holding `GET /models` for 3 s, `deepseek/deepseek-v4.1-flash-1017`: request 0 already carries `parallel_tool_calls: true`; the model issued **4 tool calls in one step**; the model-list cache was written non-empty (`models-openrouter.json` with `parallel_calls`/`reasoning_effort` fields). Mock: a listed entry without the parameter beats the family table (`s2_a_listed_entry_…`); a 400 naming `parallel_tool_calls` is retried without it and stays out of later requests (`s2_a_400_naming_…`); implementer's slow (8 s) and 503 cases pass. |
| **S3** reasoning_effort gate | **fixed** | Live, pane `live-s6b-s3`: proxy answers the first request carrying `reasoning_effort` with HTTP 400 `Unsupported parameter: 'reasoning_effort'`. Requests: #0 `medium`, #1 (retry) **no field**, #2 (next step) **no field**; an `error` note `http 400: …` is in `events.jsonl`; task completed, exit 0. Mock: a 400 that repeats after the retry ends in a failing exit with ≤ 4 requests (no loop); a 400 naming another field (`temperature`) does not drop the effort; an unlisted model gets no default effort but gets an explicit `low`; a *listed* model without the parameter gets no effort even when explicitly configured (see Notes). |
| **N1** cut notice | **fixed** | `cat <<'WANTED'` unterminated still gets the notice; marker in the middle of ordinary exit-0 output is quiet (`n1_…`). Residual: marker in the last 3 lines, or any non-zero exit with the marker, still shows it (by design in the brief). |
| **N2** PowerShell | **fixed** | see B1 row. |
| **N3** prompt snapshot | **fixed** | `tests/fixtures/prompt.md.snapshot` + `the_base_prompt_is_byte_identical_to_its_snapshot`; the live system message is 1301 bytes with the directories block. |
| **N4** CRLF | **fixed + documented** | POSIX script has no CR (checked in `cmd.sh` of the call dir); `KELIR_SHELL_RAW=cr` keeps CRLF in `cmd.sh` (`n4_raw_cr_switch_keeps_crlf`); DEVIATIONS documents both. |

## New findings

| # | sev | where | finding |
|---|---|---|---|
| N6 | **should-fix** | `src/shellcmd.rs:173-186` (`script_body`), cmd.exe family | The `.bat` has no `@echo off`, so with `shell=["cmd.exe","/C"]` every call result starts with a blank line, the prompt and the echoed command: `"\r\nC:\\…\\work>echo hello-cmd \r\nhello-cmd\r\n"` (live and in test). Before the always-file route `cmd.exe /C echo hello-cmd` printed only `hello-cmd`. For a 20 KB command the echo repeats the whole command into the result, spending tokens and flooding the spill. cmd.exe is the no-bash fallback, so impact is limited to that box type, but it is a behavioural regression of this round. Fix: emit `@echo off` as the first line (and keep CRLF). Failing proof: `n6_cmd_exe_result_has_no_prompt_echo` (ignored; `cargo test --test review_steer6b -- --ignored`). |
| N5 | **should-fix (low)** | `src/shellcmd.rs:89-99` (`command_switch`), `decide`:215 | A shell whose argv does not *end* in `-c` / `/C` / `/c` / `-Command` is not rewritten and keeps `ShellInput::Argument`, i.e. the Win32→msys argv route that halves backslash runs. Real example: `shell = ["C:/Program Files/Git/bin/bash.exe", "-lc"]` (a common login-shell spelling): 4 backslashes in a quoted heredoc reach the file as 2. The default config and `shell = "bash"` are unaffected (`-c`), so this is only a gap for custom argv. Either recognise `-lc`/`-ic`/`-ec` (any single-dash flag cluster ending in `c`), or document the limit in DEVIATIONS next to "one route". Failing proof: `s1_unrecognised_switch_keeps_the_old_route_and_its_halving` (ignored, Windows). |
| N7 | nit | `src/shellcmd.rs:224-226` | An empty/whitespace command stays on the argument route; the bash tool answers `missing non-empty command` anyway, so no effect today. Only `bg run ""` could differ. |
| N8 | nit | `src/spill.rs` (`looks_cut`) | A successful command whose *last three lines* contain "unexpected EOF" (e.g. `printf 'tail unexpected EOF'`) still gets the "may have been cut" notice (measured). Accepted by the brief; mention only. |
| N9 | nit | model behaviour, not product | In the live regression session the model sent an `edit` whose second hunk replaced only `def beta():` with a multi-line block, duplicating lines; the edit tool applied exactly what was asked ("2 changes, all applied") and the model repaired it. Not a kelir defect; recorded because it is the first live evidence that "all applied" can still be semantically wrong. |

## Regression sweep (real binary)

Mock tests in `tests/review_steer6b.rs` plus the live regression session:
- exit code 3 (bash) / 5 (cmd) / 7 (cmd, PowerShell, bg) propagate, outcome != ok; syntax error -> error; cwd of a later call is the session cwd; env var with unicode arrives; a command that reads stdin (`cat > in.txt; echo after`, and live `head -c 5 | wc -c` -> `0`) returns immediately (stdin is not the terminal); `é日本語🎉` byte-exact; a CRLF-terminated short command runs; a `\`-newline continuation joins; 100 KB heredoc (round-1 test) and 20 KB mixed-content heredoc (round-1 test) still green.
- `kelir bg run` writes `bg/<id>.sh|.bat|.ps1` (per-job names) and listed/waited correctly for 17 jobs; background promotion/cancel suites (`e2e_m4`, `review_m4`, `review_m4b`) green on Windows; on Linux m4b flaked once (above).
- `edit` multi-edit: round-1 `f3_edit_semantics` still green; live two-hunk `edit` reported `2 changes, all applied` with per-change line numbers.
- Cache prefix: live session (8 requests, proxy log): `tools` identical and the message array append-only at every step (`prefix stable: True`); `cached_tokens` 0, 1280, 1408, 1536, 1664, 1920, 2048, 2176. The `parallel_tool_calls`/`reasoning_effort` fields are the same on every request of a session (until a 400 removes one; that is a body change but not a message change).
- No process leaks from my runs: all my panes exited; `kelir.exe` instances present at the end belong to the `kelir-wt/steer5` worktree (another job), none to mine. No command window appeared (panes only).

## Notes (not defects)

- Explicit `reasoning_effort` on a model the provider lists *without* the parameter is withheld (`s3_explicit_effort_on_a_listed_model_without_the_parameter`), and the 400 retry then never fires. This matches the brief ("gate where the provider lists them"); a person who knows better has no override other than a different model entry. Worth one line in DEVIATIONS if not already there.
- Promotion was not re-driven live (the default `promote_after` is longer than my 45 s `sleep`, which ran to completion unpromoted); the Windows and Linux promotion e2e suites cover it.
- Not covered: Go gateway (no key), macOS, Store `pwsh` (known AppX/Job Object limit), `kelir bg run` past the ~32 KB argv ceiling.

## Live-drive log (Herdr panes, `C:/Program Files/Git/bin/bash.exe`, env file sourced inside the pane for model runs)

| pane | script | result |
|---|---|---|
| live-s6b-sh | `steer6b_review_shell.sh` (bash, 14 backslash lengths, unicode) | PASS, all byte-exact |
| live-s6b-cmd | `steer6b_review_shell.sh … '["cmd.exe","/C"]'` | PASS (B1), N6 echo seen |
| live-s6b-ps | same, `["powershell","-NoProfile","-Command"]` | PASS (N2) |
| live-s6b-s2 | `steer6b_review_model.sh s2` (3 s model-list delay, clean state) | PASS: first request already batched, 4 calls in one step |
| live-s6b-s3 | `steer6b_review_model.sh s3` (proxy 400 on `reasoning_effort`) | PASS: one retry, field remembered, `error` note |
| live-s6b-reg | `steer6b_review_model.sh reg` (backslash, stdin, 2-hunk edit, sleep) | PASS (behaviour); see N9 |

## Tests / scripts added

`tests/review_steer6b.rs` (16 tests: 14 pass, 2 ignored proving N5/N6); `scripts/live/steer6b_review_shell.sh`,
`scripts/live/steer6b_review_model.sh`, `scripts/live/steer6b_review_proxy.py` (delay / reject-field options; never logs headers).
