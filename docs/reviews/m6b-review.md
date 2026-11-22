# M6 round-2 review (m6b): MCP as a CLI

Reviewer: fresh QA agent. Diff: `9552b8b..d5f9a12` (the worktree branch is `job/m6`; there is no `m6b-impl-notes.md`,
the implementer notes are `docs/retros/m6fix-impl-notes.md`). Reviewer commit: `3d21bed` plus the report commit.

**Verdict: PASS WITH FIXES** (0 blockers, 2 should-fix, 3 nits). Every round-1 finding is closed on the real binary.
The broker token works. No canary escaped through any kelir channel except the one the server itself writes to
(its stderr, F1). The one real Linux gap is a server that ignores stdin EOF outliving a killed session (F2).

## Test counts

| Layer | Result |
|---|---|
| `bash scripts/gate.sh` Windows @3d21bed | fmt ok, clippy ok, nextest **1063 passed / 4 skipped**, doctest ok |
| `scripts/remote-check.sh the maintainer kelir-verify-m6b-rev` Linux @d5f9a12 | fmt, clippy ok, nextest **1057 passed (1 flaky: `e2e_steer5::a_killed_slot_holder_frees_the_slot`, STEER-9, not MCP) / 3 skipped**, doctest ok |
| `tests/review_m6.rs` (round 1) | 18/18 pass, no `ignore` left |
| `tests/review_m6b.rs` (new, 9 tests) | Windows: 8 pass + 1 ignored (`m6b-F1`). Linux with `--run-ignored all`: 7 pass, 2 fail (F1, F2). F2's test is `ignore`d off Windows so the Linux gate stays green |
| Release binary | 8.69 MB (limit 20 MB) |

## Findings

| # | Sev | Where | Finding |
|---|---|---|---|
| F1 | should-fix | `src/mcp/rpc.rs` `hint_from` (~l.448), used by the EOF error, the handshake errors and the broker's `failed: ...` server state | The server's stderr tail is shown verbatim to the model and written to the log. A server that prints its configuration when it fails to start puts its `env` value into the `kelir mcp` error text, `kelir mcp ls` and the persisted events. It is the server's output, not kelir's, but the documented guarantee is "env is never printed". Test `a_server_that_echoes_its_env_on_death_is_redacted` (ignored, `m6b-F1`) fails on Windows and Linux. Fix: before building the hint, replace every configured `env` value of 6+ bytes with `***`. |
| F2 | should-fix | `src/mcp/rpc.rs` `Client::start`, `src/proc.rs` `ProcessGroup` (Unix: `killpg` only on drop) | Linux: after `kill -9` of the session, a server that does not exit on stdin EOF, or is busy in a call and not reading, outlives the session, and so does its child. Test `a_killed_session_leaves_no_server_and_no_grandchild` fails on Linux ("the MCP server ... outlived a killed session") and passes on Windows (Job Object). A real `npx server-everything` mid-call (100 s operation) does die on Linux: node and `npm exec` exit on EOF (`scripts/live/m6b_linux_orphan.sh`: 0 left after 8 s). So round-1 F6b is narrowed, not gone. SPEC l.51: killing the parent leaves no orphans. Bash tool children already have the guard (shim polling `getppid()`, DEVIATIONS M4). Fix direction: the same watcher for MCP servers, or a small guard per server. |
| N1 | nit | `src/mcp/config.rs` `safe_error` | An unknown *key* is echoed by name (`unknown field ...`). Fine for a `TOKEN = ...` typo; a secret pasted as a bare key would show. The 34-shape fuzz (`config_errors_never_echo_a_secret`) finds no *value* echoed. |
| N2 | nit | `src/mcp/rpc.rs` `INHERITED` | The allow-list drops user `npm_config_*`, `UV_*`, `PYTHONPATH`, `VIRTUAL_ENV`, `NODE_OPTIONS`. Right for secrets; a registry set only in the environment will not reach `npx`. Say so in `docs/reference/mcp.md` (users put them in `env`). |
| N3 | nit | S1 from round 1 | A project `.kelir/config.toml` can still start a `channel = true` server at session start with no trust prompt (documented open in DEVIATIONS). |

## Round-1 findings: closure evidence

| R1 | Status | Evidence |
|---|---|---|
| F1 blocker (env printed by TOML typo) | **closed** | fuzz of 34 broken/wrong-typed shapes around a canary (`config_errors_never_echo_a_secret`), e2e `a_canary_in_env_reaches_no_output_channel`, `canaries_stay_in_their_own_channels` |
| F2 non-UTF-8 line | closed | `review_m6::a_non_utf8_line_does_not_kill_the_connection` |
| F3 stderr dropped on death | closed (now see F1 here) | `a_server_that_dies_at_start_says_why` |
| F4 nullable strings | closed | `review_m6::a_nullable_string_keeps_its_text` |
| F5 retry after send | closed | `a_lost_session_after_the_send_is_exit_4_and_not_run_again`; a bash child without the token gets exit 3, never a silent one-shot (`a_bash_child_needs_the_token_and_never_falls_back_silently`) |
| F6a inherited env | **closed** | real `server-everything get-env`: no `KELIR_*`, no provider key (PATH, SYSTEMROOT, HOME, TEMP, ... plus npm's own) |
| F6b Linux orphan | narrowed, see F2 | |
| F7 huge integer | closed | `review_m6::a_huge_integer_is_not_rounded` |
| F8 orphaned `mcp_started` | closed | `resume_settles_an_orphaned_mcp_call_as_unknown` |
| N1-N5 | closed | their tests pass |

## Broker token (STEER-7), real binary

| Case | Result |
|---|---|
| raw socket: no token / wrong / empty / prefix / token+junk / upper-cased | refused, nothing logged, server not started (`another_sessions_token_and_a_missing_key_are_refused`) |
| session B's valid token against session A's socket | refused |
| right token, `mcp-settle` / `mcp-notify` with missing, empty or guessed key, from `mcp-internal` / `mcp` / `user` | refused; no forged `mcp_settled` or `source` in the log |
| right token, `mcp` call | accepted and logged |
| bash child: normal / `env -u KELIR_TOKEN` / `KELIR_TOKEN=wrong` / empty | exit 0 / 3 / 3 / 3, only the authenticated call logged |

## Secrets (canary in env, server args, tool args, tool results)

Tests `canaries_stay_in_their_own_channels`, `the_servers_command_line_has_no_env_values`, and live run A:

- env canary: absent from NDJSON stdout, stderr, `events.jsonl`, every file of the session dir, and the server child's command line (read live). Only the server's own `leak` tool returns it, as a result.
- server-arg canary: absent from kelir output, `mcp ls --json`, `ls --detail full`, errors, events and files. It is in the server's own argv by construction, which is why secrets belong in `env`.
- tool-arg canary: only in the bash call that carried it (`tool_started` / `tool_settled`). `mcp_started.args` holds `"<string, 19 bytes>"`, verified live and in test.
- tool result: only in the bash result and the spill file. `mcp_settled` holds bytes and path only.
- Not driven: TUI scrollback screenshots. The TUI renders from the same events, which hold shapes only; I captured no screenshot.

## Live drive (Herdr panes, `deepseek/deepseek-v4.1-flash-1017`, script `scripts/live/m6b_live.sh`)

| Run | Observed | Verdict |
|---|---|---|
| A (`live-m6b-a`) | `kelir mcp ls`; `demo structured \| jq .answer` -> `42`; `s echo token=... text=hi`; real `everything get-sum a=40 b=2` -> 42; real `fs list_directory path=<scratch> \| jq -r .content` -> `[FILE] note.txt`. Three calls in one parallel batch, each with its own `mcp_started` / `mcp_settled` and `parent_call_id`. Env canary scan: none | PASS |
| B (`live-m6b-b`) | real long operation through the broker; session killed mid-call with `taskkill /F /PID <session>` (no `/T`). The npx `cmd`, both node processes, the shim and the bash tree were gone within 4 s (checked by exact PID list). My task text had a stray ` .` which the model handled | PASS (Windows) |

Real servers, one-shot on Windows (no model): `server-everything` (13 tools; get-sum, echo with unicode, enum/type validation exit 2 with a clear message, image written to a file with the path printed, progress operation, `--json -`, `--envelope`), `server-filesystem` on a scratch dir (list, read, out-of-roots read gives server `isError`, exit 1), `uvx mcp-server-time` (3 tools; a bad timezone gives exit 1). Nothing broke. Cold npx start was about 15 s under load, 3.4 s warm. (Mojibake seen with the stdlib test server is Python's cp1252 stdin, not kelir: the real server returns correct UTF-8.)

Exit codes on the real binary: 0 ok, 1 tool `isError`, 2 usage (missing tool, bad `k=v`, bad type), 3 server crash, 4 timeout ("not retried"). A 20 KB result `| wc -c` = 20020 whole; spill in a session is covered by `a_big_result_spills_and_still_pipes_whole` and round-1 live B. A channel notification becomes `source kind=channel` with the env secret absent (`a_channel_message_is_a_source_event`).

No console window: with winspy running, `kelir mcp everything ...` (npx.cmd) and a whole `kelir run` session using npx servers, both launched from a `DETACHED_PROCESS` parent, gave `visible_console_or_terminal_windows=0`. The `cmd.exe` for `npx.cmd` gets a window-less conhost.

Leaks after my runs: no `kelir.exe` left; the servers of my tests were killed by exact PID.

## Spec / brief coverage

All acceptance lines of `docs/tasks/M6.md` hold: layering, CLI shapes, exit codes, broker with lazy start and `parent_call_id`, spill, channel source events, one-shot, hand-rolled JSON-RPC (deviation recorded). Item 5 (hint / `observe` listing of servers) is still not done on this base (carried from round 1, not a blocker).
