# STEER-6 review (retro-driven loop changes F1-F7)

Reviewer: fresh QA agent. Range `9dd351f..37893f4` (implementer HEAD) on branch `job/steer6`, own target dir,
Windows 11 (Git for Windows bash) and Linux (the maintainer). Brief: `docs/tasks/STEER-6.md`.

## Verdict: FAIL (1 blocker, 3 should-fix, 4 nits) — one-line blocker, everything else solid

The retro items are implemented well and most were verified live. The blocker is the `cmd.exe` fallback of F2,
which the brief names explicitly: a command over 6000 bytes is written to `cmd.bat` but then run without `/C`,
so nothing runs and the call reports exit 0. A failing test proves it. Fix it and re-verify; no other blocker.

## Gates

| gate | result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --all-targets -- -D warnings` | clean (incl. my test file) |
| `cargo test --lib` (Windows) | 310 passed |
| `cargo test --no-fail-fast` (Windows), 3 full runs | 3 x all suites green except load flakes (below); every flake green in isolation |
| `scripts/remote-check.sh the maintainer kelir-verify-steer6-rev` (Linux, HEAD 47f2dca) | fmt ok, clippy ok, 315 unit + every suite green, exit 0 |
| `tests/review_steer6.rs` (mine) | 12 pass on Windows and Linux; 3 `#[ignore]`d tests that prove S1 and B1 (see below) |

Flake measurement (3 full runs, box shared with other agents' `cargo test`/live sessions, so noisy):
run 1 `review_m0::parallel_calls_really_overlap`; run 2 `e2e_m4::a_call_past_the_limit_is_promoted_and_wakes...` and
`e2e_verbs::model_swap_applies_at_the_next_step`; run 3 `e2e_verbs::model_swap_applies_at_the_next_step`.
Every one passed on its own (model_swap 3/3, the others 1/1). Not counted against the branch (live-session + `ctl`
timing family, STEER-9). I did not build a baseline test tree, so "same on baseline" rests on the implementer's note.

## Findings

| # | sev | where | finding |
|---|---|---|---|
| B1 | **blocker** | `src/bg.rs:840` (script branch), `src/shellcmd.rs:12,49-67` | With `shell = ["cmd.exe","/C"]` (also kelir's fallback on a Windows box with no bash), a command over 6000 bytes is written to `cmd.bat`, then the shim runs `cmd.exe <path>`: `.args(&argv[1..len-1])` drops the `/C`. `cmd.exe` without `/C` starts an interactive shell on a null stdin, executes nothing, exits 0. Result: the call settles `exit_code 0` with only the cmd banner as output; the file the command should have made does not exist. Proven by `f2_cmd_exe_runs_a_long_command_from_its_bat_file` (ignored, run with `--ignored`): the short command works, the long one "reported exit 0 but never ran". Same hazard for any switch kept by the shell that needs it (`/C`, `-Command`): `cmd.exe` must keep `/C`; for bash/sh `-c` is correctly dropped. The unit test `cmd_exe_gets_a_bat_with_crlf` only checks the file bytes, not that it runs. Fix: for the `/C` family pass `cmd.exe /C <path>` (and `/D /C` form if configured). |
| S1 | should-fix | argument route, `src/bg.rs` shim spawn (pre-existing), exposed by F2 | On Windows the **short** (<= 6000 B) route halves every run of backslashes: `\\` -> `\`, `\\\` -> `\\`, 1000 -> 500 (msys argv parsing of `bash -c <cmd>`; quotes and `$` survive). The long file route is byte-exact. So the same command means something different at 5999 vs 6001 bytes, and `printf 'a\\nb'`, `sed 's/\\//'`, regexes and Windows paths in short commands are silently altered. Reproduced on the baseline binary (`ed7b74e`) via `kelir bg run`: `a\\b` -> `a\b`; so not a regression, but the retro's "silent mangling" class is wider than the 8191 cut and the file route fixes it for free. Failing tests (ignored): `f2_backslashes_and_dollars_are_literal_in_a_quoted_heredoc_short_and_long`, `f2_a_quote_dense_command_just_under_the_threshold_is_not_cut` (5500 backslashes -> 2750 written). Suggest: on Windows route through the file whenever the command contains `\\` (or always on Windows; the cost is one small write). Not verified on Linux (the argument route there has no msys layer). |
| S2 | should-fix | `src/session.rs:597`, `src/models.rs` | F4 depends on the cached model list. The list fetch is bounded at 1500 ms and, on a miss, a `failed_at` backoff means a **first** session on a machine (or any slow fetch) sends no `parallel_tool_calls` for up to an hour, and a refreshed cache is only read at start or on a `model` verb. Observed live: through a forwarding proxy the 480-model list took > 1.5 s, the cache was written empty, and the request body had no `parallel_tool_calls` although `deepseek/deepseek-v4.1-flash-1017` advertises it; with a warm cache the field was sent (`true`) and the model batched 4 reads in one step. Acceptable as designed, but the brief's "F4" benefit silently depends on warm state; worth a line in DEVIATIONS or a longer first-fetch timeout. |
| S3 | should-fix | `src/provider.rs:124`, `src/config.rs` (default `medium`) | `reasoning_effort` is now sent unconditionally to every model and provider, with no capability gate (OpenRouter advertises `reasoning_effort` per model in `supported_parameters`) and no fallback if a gateway answers 400 to the unknown field. A single provider/model that rejects it makes every request a hard error until the user finds `reasoning_effort = "none"`. Go could not be probed (no key, see live log). Suggest: gate on `supported_parameters` where the provider lists it, or retry once without the field on a 400 that names it. |
| N1 | nit | `src/spill.rs:199-200,228` | The truncation notice triggers on any output containing `unexpected EOF`/`delimited by end-of-file`, including ordinary output (`git grep "unexpected EOF" src` finds spill.rs itself, a Go test log, a book). The model then reads "your command may have been cut short" for a command that was not. Low harm, slightly misleading; consider requiring a non-zero exit or the marker in the last lines only. |
| N2 | nit | `src/shellcmd.rs:49-56` | `command_switch` accepts `-Command`/`-command`, but `script_name` gives `cmd.sh` and the shim drops the switch, so `pwsh`/`powershell` would be asked to run `cmd.sh`. Untested (my pwsh probe died earlier in an unrelated `cannot assign it to the job: Access is denied` job error, so I could not conclude). Either handle PowerShell (`-File cmd.ps1`) or leave `-Command` out of the rewrite list. |
| N3 | nit | brief "snapshot test" / `src/sources.rs:719` | The brief asks for a snapshot test of the rendered system text (deterministic bytes). The added test pins rule substrings and the existing test pins `<= 1024` bytes (1014 now), not the bytes. I covered determinism in `f1_the_system_message_is_deterministic_and_the_prefix_is_stable`, but a byte-exact snapshot of the embedded prompt is still missing. |
| N4 | nit | `src/shellcmd.rs:70-75` | A long command is normalised `\r\n` -> `\n` for the `.sh` file, so an intentional CR inside a heredoc payload is lost on the long route (and kept on the short route). Arguably right (CRLF terminators otherwise break heredocs: my CRLF test passes), but undocumented. |

## What I verified, by acceptance line

**F2 (long commands), highest value.**
- Unit/e2e: 40 KB heredoc (implementer's) plus mine: **100 KB** heredoc byte-exact (`f2_a_100_kb_heredoc_writes_every_byte`); a 20 KB heredoc full of quotes, `$HOME`, backticks, `$(...)`, `\`, `%PATH%`, `!`, é, 日本語 and an emoji comes back byte-for-byte; CRLF command (terminator intact, 400 lines); lengths 5900, 5999, 6000, 6001, 6100, 8000, 8190, 8191, 8192, 8300, 9000, 16000 each write their full payload; a syntax error in a long command is `error`, exit != 0; **cwd, env var (with unicode) and exit code 7 are preserved**, a later short call starts back at the session cwd; a 5000-`"` command just under the threshold is intact (Rust's quoting survives; only backslash runs break, S1).
- Live (Herdr pane, OpenRouter `deepseek/deepseek-v4.1-flash-1017`, real binary): the model produced one `bash` call of **67,432 bytes** (spilled as `blobs/6-input`); `gen.txt` has exactly 900 lines; `commands/6-1-1/cmd.sh` holds the command; `tool_settled ok exit 0`.
- `cmd.exe` fallback: broken, B1.

**F1 (reasoning effort, prompt).**
- Mock body: `reasoning_effort` present and equal to `medium` by default, `low`/`high` from config, absent for `none`, also on the compaction request (`f1_the_effort_is_also_sent_on_the_compaction_request`), recorded in `session_started.reasoning_effort` (live log shows `medium`).
- Base prompt 1014 B (<= 1 KB budget); the rendered system message and `tools` array are byte-identical between steps and the messages array is append-only (`f1_the_system_message_is_deterministic_...`; the directories list is the only per-run difference because it names the temp home).
- **Live on OpenRouter** (`scripts/live/steer6_review_effort.sh`, one reasoning-heavy prompt, `max_tokens` 6000, `completion_tokens`, 3 requests per arm, 4 for medium):

  | arm | completion tokens |
  |---|---|
  | no field | 5189, 6000 (cap), 3982 |
  | flat `reasoning_effort: low` | 1787, 2788, 2023 |
  | flat `medium` (kelir's default) | 2852, 3481, 1539, 2475 |
  | flat `high` | 6000 (cap), 4292, 4260 |
  | `reasoning: {effort: low}` | 1243, 2335, 5375 |
  | `reasoning: {effort: high}` | 6000 (cap), 4816, 3241 |

  The flat field is **honoured on OpenRouter** for this model: low about 55 % and the default medium about 45-50 % below the no-field default, and `high` stays at the cap; the object form is noisier. So the new default `medium` does cut reasoning here. **OpenCode Go: not live-verified** (no `KELIR_GO_API_KEY`); the implementer's own Go measurement ("honoured, weak") is taken from their notes, not reproduced. See S3.
- Side note: my first warm-up run used the machine's default config and reached the OpenCode Go gateway once (a one-line "say ok" with an OpenRouter-style model id, rejected with HTTP 400, no spend). All later runs used `--config <empty>`.

**F3 (multi-hunk edit).** `f3_edit_semantics` (one session, 15 calls): order-dependent hunks (hunk 2 matches hunk 1's output); a missing last hunk leaves the file byte-identical; an ambiguity introduced by hunk 1 makes hunk 2 fail and leaves the file untouched; `replace_all` inside a hunk; CRLF file edited with LF patterns keeps CRLF on every touched line; both forms at once, empty `edits`, missing `new`, `edits` as a string, a non-UTF-8 file, an empty `old` on an existing file all error and leave the file alone; create + refine in one call; create with a failing second hunk does not create the file; a replacement containing the pattern with `replace_all` does not loop. All pass. Live: given "rename alpha/beta/gamma and change VALUE", the model used **one `edit` call with 4 `edits`**, "4 changes, all applied", then `python m.py` ran. The prompt line ("prefer it to a python/sed patch, which can fail with exit 0", "never chain `patch && cargo test`", "check exit codes") is present.

**F4 (parallel calls).** Body has `parallel_tool_calls: true` only when the model's `supported_parameters` lists it and the provider is OpenRouter; absent when not advertised, when the model is not in the list, when the list endpoint fails (500), and for Go (unit test). Live (warm model cache): body `true`, accepted by OpenRouter, the model issued all four independent reads in one step (4 tool calls, one step). Without the warm cache the field was absent: S2. Note `models::lookup` also matches on the last path segment, so an unrelated `other/model` matches `test/model`; pre-existing.

**F5 (soft compaction).** Arithmetic: unit tests cover min(window*0.8, target), small window keeps its fraction, 0 disables the cap, nonsense fraction falls back to the target. Mine, on the real binary with window 1,048,576: a step reporting **149,999** prompt tokens does not compact, **150,000** does (`f5_the_soft_target_boundary_is_exact`); `compact_at_tokens = 0` with 400,000 tokens does not compact. Live (`compact_at_tokens = 6000`, real model): steps grew 1227, 2695, 4173, 5651, 7129 prompt tokens; the compaction fired after the step that reported 7129 (`by: auto`, ~4676 -> ~2438 estimated tokens), the next request had 4521 prompt tokens with `cached_tokens` 1152 (the frozen system+tools prefix; "cache restarts" as designed) and the step after it 5999 with **4480 cached (75 %)**. No compaction loop. Behaviour change worth knowing: a window that resolves to 0 with a target now compacts at the target instead of never (`auto_threshold`, `src/compact.rs:64`); harmless.

**F6/F7 (prompt lines).** Present and pinned by `the_embedded_prompt_carries_every_retro_rule` ("name the proving command", `git grep`, `target/`, "never `cat` a big file", "share no shell state"). 1014 B, `{{skills_dirs}}` kept.

**Cache prefix / regressions.** System message, tool specs and the earlier messages are unchanged between steps (my prefix test, and the live cached_tokens climb: 0, 1152, 1280, 1536 on the edit session). No new dependency (`Cargo.toml` untouched), no personal data in the diff (machine names appear only as the `remote-check.sh` argument in the notes). Release binary built clean (2 min); size not re-measured. `tasklist`/CIM check after my runs: no leftover process of mine (the proxy python processes died with their pane scripts; I stopped one background `cargo test` loop by task id, not by image name).

## Live-drive log (all in Herdr panes, OpenRouter, `deepseek/deepseek-v4.1-flash-1017`, env file sourced in the pane)

| scenario | command | observed | verdict |
|---|---|---|---|
| effort probe | `steer6_review_effort.sh` (curl, 3-4 per arm) | table above | PASS (OpenRouter), Go not live-verified |
| heredoc | `steer6_review_live.sh heredoc` | one 67,432 B bash call, 900 lines written, `cmd.sh` kept, exit 0, effort `medium` in body and in `session_started` | PASS |
| edit | `... edit` | one `edit` with 4 `edits`, file correct, cache 0 -> 1152 -> 1280 -> 1536 | PASS |
| batch | `... batch` (cold list, then warm list) | cold: no `parallel_tool_calls` (S2); warm: `true`, 4 calls in one step | PASS WITH S2 |
| compact | `... compact` with `compact_at_tokens = 6000` | auto compaction after the 7129-token step, cache recovers to 75 % two steps later | PASS |

No TUI is involved in this job, so no screenshots were taken. Not covered: Go gateway behaviour, `kelir bg run` with a command past ~32 KB (OS argv ceiling, recorded in DEVIATIONS), macOS, a resumed session across a soft-target compaction.

## Tests added (all in `tests/review_steer6.rs`, scripts in `scripts/live/steer6_review_*`)

`f1_*` (2), `f2_*` (6 pass + 3 ignored proving S1/B1), `f3_edit_semantics`, `f4_*` (1), `f5_*` (2). Run the ignored ones with
`cargo test --test review_steer6 -- --ignored`; they fail today on Windows by design (S1 x2, B1) and should turn green after the fixes.
