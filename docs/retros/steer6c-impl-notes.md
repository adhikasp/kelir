# STEER-6 fixes, round 2 — implementation notes (job steer6c)

Brief: `docs/tasks/STEER-6-fixes2.md`. Findings: `docs/reviews/steer6-review-round2.md`
(PASS WITH FIXES: 0 blockers, 2 should-fix N5/N6, 4 nits), pointer `docs/reviews/steer6b-review.md`.
Both should-fix items are fixed and verified on the real release binary; of the nits, N10 (the
per-route CRLF policy) is fixed as a one-liner in behaviour, N7/N8/N9 are recorded as accepted.

## 0. Merged `main`

`git merge main` first, as the brief requires. Two conflicts, both from the M5-A fixes that landed
on main in the meantime:

- `src/sources.rs`: both sides added a test at the end of the module (this branch's N3 prompt
  snapshot, main's "the embedded base prompt has no path to read"). Kept both — they check
  different things.
- `docs/PROGRESS.md`: two rows appended at the same place. Kept both rows.

`cargo build --lib` after the resolution, then the merge commit, before touching any code.

## 1. N6 (should-fix) — `cmd.exe` echoed the whole command into the result

`script_body` now puts `@echo off` as the first line of every `.bat`. Measured on this box before
the fix: `echo hello-cmd` came back as `"\r\nC:\...\work>echo hello-cmd \r\nhello-cmd\r\n"` —
a blank line, the prompt, the echoed line, then the output; a 20 KB command echoed ~3.7 KB back
(cmd.exe's own line limit splits it) on top of the output. After the fix the same call is
`"hello-cmd\r\n"`.

Worth recording: the echo is gone only because `@echo off` is the very first line — `rem ...` as
the first line keeps echoing, and a `rem` line past cmd.exe's ~8 KB parser limit is still split
and partly echoed *by cmd.exe itself*, not by kelir. Probe table (100…20000 `x`s, with and
without `@echo off` first) in the session log; the split is cmd.exe's, and the test asserts what
kelir owns (no prompt, no blank line, and well under the echo size the bug produced).

Exit codes, cwd, `\`, `%%` are unchanged (`b1_cmd_exe_short_long_exit_cwd` still green, now with
the N6 assertions added). `@echo off` is only for the `cmd.exe` family: PowerShell and POSIX
scripts are untouched.

## 2. N5 (should-fix) — `bash -lc` kept the halving route

`command_switch` now returns a `Switch`, not a `&str`:

- `-c` → `Posix { flags: "" }`;
- a short-flag cluster `-XYc` (one dash, ASCII letters, ending in `c`) → `Posix { flags: "-XY" }`,
  so `bash -lc` becomes `bash -l cmd.sh` (the login flags stay, the `c` is the one the path
  replaces). `-ic`, `-ec`, `-xc`, `--login -ic` all take the file route;
- `/C`, `/c` → `Cmd`, kept verbatim (B1);
- `-Command`/`-command` → `PowerShell`, replaced by `-File` (N2).

Everything else is still not kelir's argv to reinterpret, but what happens next now depends on
whether the argument route is safe:

- on Unix it stays exactly as configured (no msys layer, no 8 KB cut): `decide(.., false)`;
- on Windows (`always_file`) a **non-empty** command for such a shell is an **error** that names
  the argv and the spellings that work. Silent halving was the whole defect; silently leaving the
  route in place would have kept it.

An empty command still takes the argument route on both platforms, because the tool answers
`bash: missing non-empty command` before anything runs (N7).

Live, real release binary, `kelir bg run`, `shell = ["C:/Program Files/Git/bin/bash.exe", "-lc"]`:
a quoted heredoc's 4 backslashes arrive as 4 bytes (`r.txt` = `\ \ \ \ LF`), and `a\b` arrives
as `a\b`. Before the fix it was 2 and `a<backspace>`-territory.

The reviewer's `s1_unrecognised_switch_keeps_the_old_route_and_its_halving` (ignored, failing) is
now `n5_a_login_shell_cluster_takes_the_file_route` (green). It configures the *resolved* default
shell with `-c` replaced by `-lc` rather than a bare `"bash"`: on this box `bash` on PATH is a WSL
launcher, and the first version of the test failed for that reason, not for N5. The reviewer's
`n6_cmd_exe_result_has_no_prompt_echo` is un-ignored and green.

## 3. N10 (nit) — the CRLF policy depended on the route

The normalisation now follows the **shell family**, not the route: `decide` normalises CRLF to LF
inside the argument it hands a POSIX shell, exactly as `script_body` does for the file, so a
command means the same below and above `SHELL_CMD_MAX` (`echo a > f<CR>` wrote `f<CR>` under it and
`f` above it; the reviewer's test passed on Windows and failed on Linux for this). `KELIR_SHELL_RAW=cr`
keeps the raw bytes on both routes. The reviewer's `s1_short_command_regressions` CRLF block now
asserts the same thing on both platforms, and a unit test pins the pair of routes.
`DEVIATIONS.md` says so.

The nits left as they are: N7 (empty command, no effect today), N8 (a marker in the last three
lines of a successful command still gets the notice — the brief asked for that), N9 (model
behaviour, not kelir).

## Tests

- `src/shellcmd.rs`: 16 unit tests (2 new: the cluster's file-route argv for `-lc`/`-ec`/`-xc`/
  `--login -ic`, and the N10 pair; the "no command switch" test now covers both platforms and the
  error text; `every_length_keeps_the_command_byte_exact` asserts `@echo off` first and that the
  POSIX form never carries it).
- `tests/review_steer6b.rs`: the two ignored N5/N6 proofs are un-ignored (16 pass, 0 ignored), and
  the cmd.exe tests assert the echo is gone and that a 20 KB command does not come back as echo.

## Gates

| gate | result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --all-targets -- -D warnings` | clean |
| `cargo test --no-fail-fast` (Windows, one full run) | 22 suites, every one green: 334 unit + e2e 16 + e2e_m3 11 + e2e_m4 10 + e2e_m5 10 + e2e_steer4 6 + e2e_steer6 14 + e2e_tui 17 + e2e_verbs 23 + review_m0 8 + review_m1 5 + review_m2 2 + review_m3 11 + review_m4 19 + review_m4b 7 + review_m5a 20 + review_steer1 6 (2 ignored) + review_steer4 9 + review_steer6 15 + review_steer6b 16 |
| `scripts/remote-check.sh <machine> kelir-verify-steer6c` (Linux, HEAD b736fb2) | fmt clean, clippy clean, 339 unit, every suite green, exit 0 |

Live drive (real release binary, no model; `scripts/live/steer6b_review_shell.sh`, env scrubbed of
this session's `KELIR_*` so the jobs land in the scratch state dir):

| pane/kind | config | result |
|---|---|---|
| bash | default (`bash … -c`) | 14 backslash lengths 1…9000 all byte-exact; unicode `é日本語🎉` intact; `bg` exit 7 recorded |
| bash | `<git>/bin/bash.exe -lc` (N5) | 4 backslashes in a quoted heredoc = 4 bytes; `a\b` intact |
| cmd.exe | `["cmd.exe","/C"]` (N6) | `short.txt` written, result `hello-out` only — no prompt, no echo, no blank first line; `exit /b 7` recorded; 20 KB command ran (`long-ran`) |
| powershell | `["powershell","-NoProfile","-Command"]` | `-File cmd.ps1`, short and 20 KB commands ran, `exit 7` |

No process leaks: `tasklist` shows no `kelir.exe` after the runs; the live jobs were waited for in
their own scratch state dirs.

## Known gaps

- On Windows, a shell kelir cannot rewrite is now an error for every non-empty command, including a
  short one that would have survived the argument route. That is deliberate (no silent halving),
  and the message says which spellings work.
- The `@echo off` line is one more line in the script; a model that reads `cmd.bat` sees it. It is
  the same file the shell runs, which is the point.
- Nothing else changed: S1/S2/S3/B1/N1/N2/N3/N4 stay as fixed in round 1, and the round-2 review's
  regression sweep (exit codes, cwd, env, stdin, unicode, 100 KB, edit atomicity, cache prefix) was
  re-run through the suites.
