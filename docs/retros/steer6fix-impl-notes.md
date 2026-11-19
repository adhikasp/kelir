# STEER-6 fixes — implementation notes

Brief: `docs/tasks/STEER-6-fixes.md`; findings: `docs/reviews/steer6-review.md` (FAIL, 1 blocker).
All of B1, S1, S2, S3, N1, N2, N3, N4 are addressed; F1–F7 (from STEER-6) are unchanged except
where a fix touched them.

## In the brief's order

### 0. Merged `main`

`git merge main` (STEER-4, M5-A, M5-A fixes, the STEER-5/6/10/11 briefs, the TUI v2 design note),
no conflicts (docs only on main). Committed as a merge on `job/steer6` before anything else.

### 1. B1 — `cmd.exe` dropped `/C` and ran nothing

`src/shellcmd.rs` now decides the argv a script needs, not just the path:
`ShellInput::Script { path, keep_switch }`. `cmd.exe /C` is kept (`cmd.exe /D /C <path>` keeps
`/D` too), because `cmd.exe <path>` starts an interactive shell on a null stdin, executes nothing
and exits 0 — the blocker. A POSIX shell still drops its `-c` (it takes the file positionally).
`script_input()` covers the path where a caller already wrote the file (the bash call and
`bg run` write it before the shim exists), so the switch follows the shell in both places.
The reviewer's `f2_cmd_exe_runs_a_long_command_from_its_bat_file` is un-ignored and green; a unit
test pins `cmd.exe /C <path>` and `cmd.exe /D /C <path>`.

### 2. S1 + N2 — one route per platform; PowerShell handled

- **Windows: always a file.** `shellcmd::always_script()` is `cfg!(windows)`, so a command of any
  length takes the script route there and never crosses the Win32 → msys argv chain that halved
  every run of backslashes (`a\\b` → `a\b`). The threshold no longer exists on Windows, so a
  command's meaning cannot change at a length boundary. `input()` is now a thin wrapper over
  `decide(.., always_file)`, so both routes are unit-tested on both platforms.
- **Unix: `-c` for short commands**, the file route above `SHELL_CMD_MAX = 6000`. There is no
  msys layer there, and the file route is the one a long command always used; the whole suite
  exercises it on Linux.
- **Property test**: `every_length_keeps_the_command_byte_exact` runs the pure body transform
  (`script_body`) over lengths 1..=9000 with a growing backslash run, quotes, `$`, backticks,
  unicode and `%`; the POSIX and `cmd.exe` forms are checked at every length (LF vs CRLF, no
  `\r\r`, no `\n\n`, payload unchanged). Two e2e tests cover the same bytes through the real
  binary on Windows: the reviewer's backslash/quoted-heredoc and quote-dense cases.
- **N2 — PowerShell**: `-Command`/`-command` now yields `cmd.ps1` and `-File <path>`;
  `-Command <path>` would have run the path as PowerShell source. Unit-tested for the file bytes
  (CRLF, one terminator) and the argv (`-NoProfile -File cmd.ps1`). Live: e2e
  `a_powershell_shell_runs_its_ps1_through_minus_file` drives `shell = ["powershell",
  "-NoProfile", "-Command"]` through the real binary and checks the script really writes its
  output. Finding on the way: a **Store** `pwsh` cannot be a call shell at all — it is an AppX
  execution alias and Windows refuses to put the alias's process in a Job Object
  (`cannot assign it to the job: Access is denied`), the same wall the STEER-6 reviewer hit. Not
  kelir's bug; the inbox `powershell.exe` works, and DEVIATIONS records the limit.
- **Exit code, cwd and env** are unchanged: the shim still spawns the shell with the same stdio,
  cwd and env; the reviewer's `f2_cwd_env_and_exit_code_are_preserved` still passes.
- **N4 — CRLF, documented**: POSIX scripts get LF (a CRLF `cmd.sh` breaks a heredoc terminator);
  `cmd.exe`/PowerShell get CRLF; a lone `\r` in a payload is untouched (a unit test writes one to
  a file and reads it back). `KELIR_SHELL_RAW=cr` keeps the raw CRLF terminators, which is the
  cheap escape hatch for a caller that needs them.
- Job scripts are now `bg/<id>.sh|.bat|.ps1`, not one shared `bg/cmd.sh`: two `bg run`s could
  overwrite each other's script (found while making `bg run` take the file route on Windows).

### 3. S2 + S3 — capabilities do not depend on a warm list

- `models::parallel_calls(list, model)` and `models::reasoning_effort_allowed(list, model,
  explicit)` are the two decisions, unit-tested.
  - Batching: the provider's entry wins; else a conservative family table
    (`deepseek/`, `anthropic/`, `openai/`, `gpt-4`, `gpt-5`, `google/`, `gemini-`, `qwen/`,
    `kimi`, …); else no. A first session with a slow or dead list still asks
    `deepseek/deepseek-v4.1-flash-1017` and still refuses an unknown model — e2e
    `parallel_tool_calls_does_not_wait_for_a_slow_or_missing_list` (a slow list is held 8 s
    against the 4 s bound, and a 503 endpoint).
  - Effort: sent where the list advertises it (either `reasoning_effort` or the `reasoning`
    object form), not where the list has the model without it, only when explicitly configured
    for a model the list does not know, and still sent when the provider lists no capabilities
    at all (Go). `reasoning_effort_explicit` in the config is what separates the default
    `medium` from a person's choice.
  - `session_started.reasoning_effort` records what is actually sent, so log and request agree
    (a gated model records no field).
- A slow `/models` fetch is no longer stored as a failure (`FETCH_TIMEOUT` 4 s, `Err` only on a
  real error): a 2 s proxy answer no longer leaves the cache empty for `RETRY_AFTER_MS`.
- A 400 whose body names `reasoning_effort` or `parallel_tool_calls` is retried once without it,
  recorded as an `error` note ("retrying without `…` and leaving it out for the rest of the
  session"), and remembered in `Session::refused_fields` so later requests never carry it. The
  retry does not spend the provider attempt budget. e2e
  `a_400_naming_the_effort_is_retried_once_without_it` asserts three requests, the drop, the
  note, and the tool call still running.

### 4. N1 + N3

- **N1**: `spill::looks_cut` needs evidence: a shell diagnostic line (`bash: warning: …delimited
  by end-of-file…`, `sh: -c: line 1: unexpected EOF …`), or a non-zero exit code, or the marker in
  the last `NOTICE_LINES = 3` lines of a successful command. The bash heredoc warning is printed
  *before* the body it did read, so position alone would have missed the real case. A `git grep`
  for the markers, hits in the middle, is now quiet (unit test built from the review's example).
- **N3**: `tests/fixtures/prompt.md.snapshot` pins the base prompt byte-for-byte, compared by
  `the_base_prompt_is_byte_identical_to_its_snapshot` (which also asserts the rendered base is
  deterministic and still ≤ 1 KB). The rule list test stays.

### 5. Docs

`docs/DEVIATIONS.md` rewrites the shell entry (one route per platform, switch kept, CRLF policy),
and adds the S2 batching rule, the S3 effort gate + 400 retry, the N1 evidence rule and the N3
snapshot rule. No public data, hostnames or keys were added; machine names appear only as the
`remote-check.sh` argument in these notes.

## Tests added or changed

- `src/shellcmd.rs`: 14 unit tests (route per platform, threshold, `cmd.exe /C`, `/D /C`,
  PowerShell `-File`, CRLF, lone CR, job naming, property sweep 1..=9000).
- `src/models.rs`: 4 new (family table, batching fallback, effort gate, conservative table);
  `the_openrouter_shape_is_read` now checks both capabilities.
- `src/config.rs`: `an_explicit_effort_is_told_apart_from_the_default`.
- `src/spill.rs`: 3 new (middle-of-output quiet, shell diagnostics anywhere, ordinary output
  quiet).
- `src/sources.rs`: the snapshot test.
- `tests/e2e_steer6.rs`: 3 new (slow/missing list batching, 400 retry, gated default effort).
- `tests/support/mod.rs`: `ModelsAnswer::slow` and a `delay` field; `test/model` now advertises
  `reasoning`/`parallel_tool_calls` like the OpenRouter model it stands in for (the gate is
  honest, so the fixture has to be).
- `tests/review_steer6.rs`: the three ignored tests (B1, S1 ×2) are un-ignored and green; the
  comments that described the bug are updated.

## Gates (Windows dev box)

| gate | result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --all-targets -- -D warnings` | clean |
| `cargo test --lib` | 325 passed |
| `cargo test --no-fail-fast`, 3 full runs | see PROGRESS row; known flakes are `review_m4`
  r18/r19 (documented) and `e2e_verbs::model_swap` (STEER-9) |
| `scripts/remote-check.sh <machine> kelir-verify-steer6fix` | see PROGRESS row |

One test bug found and fixed on the way: an early status write in the shim's script branch made
`bg run` read a status with no pid (`review_m4::r3` caught it). The fields are set in memory and
written once after the spawn, as before.

## Still open (not this brief)

- `kelir bg run <cmd>` past ~32 KB is still an OS argv ceiling (DEVIATIONS).
- Store `pwsh` as a call shell: blocked by AppX alias + Job Object (DEVIATIONS); the inbox
  `powershell.exe` is verified.
- `models::lookup` still matches on the last path segment, so `other/model` matches `test/model`
  (pre-existing, noted by the reviewer; a fix is a separate change).
