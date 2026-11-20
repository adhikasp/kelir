# M6 review: MCP as a CLI

Reviewer: fresh QA agent. Diff reviewed: `ed21173..9552b8b` (branch `job/m6`). Verdict: **PASS WITH FIXES**
(1 blocker, 7 should-fix, 5 nits, 1 security note). The core design is sound; the blocker is a documented guarantee
("`env` is never printed") that a TOML typo breaks.

## Test counts

| Layer | Result |
|---|---|
| `bash scripts/gate.sh` on Windows @9552b8b | fmt ok, clippy ok, nextest **961 passed / 3 skipped**, doctest ok |
| `scripts/remote-check.sh the maintainer kelir-verify-m6-rev` (Linux) @9552b8b | fmt, clippy, nextest **961 passed / 3 skipped**, doctest ok |
| `kelir docs --check` | up to date (13 topics); the drift test is part of the 961 |
| `tests/review_m6.rs` (new, reviewer) | 18 tests: **14 pass, 4 fail**. The 4 are `#[ignore]`d with the finding id so the gate stays green; run `cargo nextest run --test review_m6 --run-ignored all` |
| Release binary | 8.4 MB (limit 20 MB) |

Not run on Linux: the review tests (the bundle carries HEAD only). Not verified: Linux orphaning of a real node server
after `kill -9` of the session (F6b).

## Findings

| # | Sev | Where | Finding |
|---|---|---|---|
| F1 | **blocker** | `src/mcp/config.rs:75` | A TOML syntax error echoes the offending source line, including the `env` block. `env = { API_TOKEN = "sk-SECRET-VALUE-123" , } oops` prints that line verbatim to stderr and into `config_error` of `kelir mcp ls`, so it reaches the model and the persisted event log. The docs promise "never printed". Test `a_bad_mcp_toml_does_not_print_the_env_block` fails. Fix: print only line:col and the message, never the snippet. |
| F2 | should-fix | `src/mcp/rpc.rs:407` | `lines.next_line()` returns `Err` on a line that is not valid UTF-8 and `while let Ok(Some(..))` treats that as EOF: the server is declared dead (all waiters get "the server exited") although it is running. A stray byte in a log line on stdout kills the connection (exit 3). Test `a_non_utf8_line_does_not_kill_the_connection` fails. Fix: read bytes and use lossy decoding or skip. |
| F3 | should-fix | `src/mcp/rpc.rs:215-229, 462-471` | When a server dies during start (bad path, missing API key), the CLI prints only `the server exited`; the server's stderr, the one place it says why, is dropped (the read-loop EOF error has no `stderr_hint()`). Repro: `args=["C:/nonexistent/server.py"]` and a server that writes "boom: missing API key" then exits 1 both print just `kelir mcp: <name>: the server exited`. In the live drive a model burned ~40 steps debugging a bad path for this reason. Fix: append the stderr tail (after a short drain wait) to the read-loop and `handshake` errors. |
| F4 | should-fix | `src/mcp/args.rs:135` | `declared_type` only knows `type: "<one string>"`. A property declared `anyOf:[string,null]` or `type:["string","null"]` (common from zod/pydantic) is not "declared string", so `a=123` is sent as the number 123, and the validator ignores `anyOf`, so nothing catches it. Test `a_nullable_string_keeps_its_text` fails for `a`. Fix: treat a property as string when `type` is or contains `string`, or any `anyOf`/`oneOf` branch is `string`. |
| F5 | should-fix | `src/mcp/cli.rs:132-136, 232` | `ask()` maps every transport error to `Ok(None)` and the caller then silently runs the call one-shot. A failure after the request was sent (session died or closed the socket mid-call) re-executes a possibly non-idempotent tool, against "unknown outcome, never retried". Only a failed connect may fall back. Found by reading, not reproduced. |
| F6 | should-fix | `src/mcp/rpc.rs:131` | (a) The server inherits the whole session environment: `server-everything get-env` showed it received `KELIR_OPENROUTER_API_KEY` and `HC_*`; only `KELIR_SOCK/CALL_ID/SESSION_DIR` are removed. Third-party `npx` packages should not get provider keys; use an allow-list (PATH, HOME, TEMP, SystemRoot, as the MCP SDKs do) plus the server's `env`. (b) Linux: no `PDEATHSIG`, so after `kill -9` of the session a server lives until it notices stdin EOF; the TS SDK stdio transport historically does not exit on EOF. Untested with a real node server on Linux. |
| F7 | should-fix | `src/mcp/args.rs:149` | An untyped `k=v` integer beyond u64 (`d=123456789012345678901234567890`) is parsed to f64 and sent as `1.2345678901234568e+29`: silent corruption of an id. Test `a_huge_integer_is_not_rounded` fails. |
| F8 | should-fix | `src/session.rs` (`mcp_request`, resume) | A session killed mid-call leaves `mcp_started` with no `mcp_settled` forever; after `--resume` the bash call settles `unknown` but the MCP call never does. Repro: canned model runs `kelir mcp demo slow seconds=60`, `taskkill /F` the session, resume. Resume should write `mcp_settled outcome=unknown`. |
| N1 | nit | `src/mcp/broker.rs:74` | The broker reads `mcp.toml` once at session start while the one-shot path re-reads it; editing the file mid-session has no effect (the live model lost many steps to this). Document it in `docs/reference/mcp.md` or reload per request. |
| N2 | nit | `src/mcp/rpc.rs:449` | `notifications/tools/list_changed` is ignored, so the tool cache (also used for validation) is stale for the whole session. |
| N3 | nit | `src/mcp/args.rs:70` | A tool literally named `ls` is unreachable (`kelir mcp srv ls` always lists). |
| N4 | nit | `src/docs/tables.rs` (mcp rows), `src/verbs.rs` (tools error) | Backslash continuation lost: runs of ~20 spaces inside the event table text in `docs/reference/events.md` and in the `tools:` error message. |
| N5 | nit | `src/mcp/rpc.rs:295-305` | Race: a request whose `is_alive()` check passes just before the reader sees EOF is inserted after the waiters were drained and waits the full timeout, giving exit 4 ("unknown outcome") for a server that visibly died (should be 3). |
| S1 | security note (spec-mandated) | `src/mcp/config.rs` `project_path` | The project `.kelir/mcp.toml` can name any `command`, and `channel = true` starts it at session start with no prompt, so opening a cloned repo runs its command. The spec asks for the layering; consider a trust gate (STEER-7 has `src/trust.rs`) before auto-starting project-defined servers. |

## STEER-7 (session token): what must change when merged

Today the socket has no authentication (`src/ipc.rs` has no token anywhere); the `mcp` verb is as open as `view`/`bg`.
On `job/steer7` the check is at the listener (`Trust::accepts`) and `request_value` adds `KELIR_TOKEN` from the environment.
For M6, exactly:

1. `src/ipc.rs`: `request_value_within` (new in M6) calls `connect_value` + `round_trip` directly and bypasses steer7's token
   injection in `request_value`; the `mcp` verb would be refused with "carries no token". Add
   `with_token(trust::token_from_env())` there too. `round_trip`'s signature differs between the branches (M6 adds a
   `wait` argument): a merge conflict.
2. `src/mcp/broker.rs:155,181`: the `Incoming { request, reply }` literals need steer7's `action: None`.
3. `src/mcp/cli.rs:138-146`: a refusal arrives as `!response.ok` and already becomes exit 3 with the message (it does not fall
   back one-shot): keep that, and fix F5 so a transport error after the send does not fall back either.
4. `src/mcp/rpc.rs:131`: add `KELIR_TOKEN` to the variables removed for servers (or use the F6 allow-list).
5. `mcp-settle`/`mcp-notify` travel in-process, not through the listener, so the token check does not touch them; their random
   per-session key remains the second layer (a socket client then needs token and key to forge one).
6. `tests/e2e_m6.rs::a_socket_client_cannot_forge_a_settlement` and any test that talks to the socket directly must send the token.
7. `src/session.rs` `mcp_verb`/`mcp_request`: steer7 reshapes `session.rs`; re-apply the two intercepts. `mcp` runs a tool, so
   it must stay behind the token check and not be added to any unauthenticated-verb list.

## The hand-rolled JSON-RPC subset (instead of `rmcp`)

Sound for tools-only stdio servers; I would accept the deviation. Checked against `server-everything` (13 tools, `.cmd` shim),
`server-filesystem` (real `npx -y`, 20 s cold) and the evil server (`tests/support/mcp_evil.py`):

| Aspect | Result |
|---|---|
| Version negotiation | sends 2025-06-18, accepts any answer incl. 2024-11-05 (test); does not check the answered version (fine for this subset) |
| Capabilities / initialized | `capabilities:{}`, `notifications/initialized` sent |
| Notifications, ping, server-to-client request | progress, `roots/list` (answered -32601), ping: ok (test) |
| Partial lines / CRLF / junk banner | ok (tests); **non-UTF-8 line: F2** |
| Cancellation | `notifications/cancelled` on timeout, server left running |
| Large messages | 12 MB and 50 MB through the broker (50 MB in 3 s, `wc -c` = 52428800, `mcp_settled.bytes` = 52428800); no size cap; memory not measured precisely (1 s samples are too coarse to trust) |
| stderr | an 8 MB flood does not block (drained); content dropped on death: F3 |
| Crash / restart | dead server is exit 3, next call restarts it (e2e) |
| Hang on `initialize` | exit 3 after exactly 60 s, no leftover process |
| `.cmd` / `npx` | `command = "npx"` and a direct `.cmd` path both work; `CREATE_NO_WINDOW` is set when the process has no console (`proc.rs`). I did not capture a screenshot of a console-less run |
| Tree cleanup | `taskkill /F` of the session: server gone (Job Object); process listings after the runs were clean |
| Not implemented | HTTP/SSE, sampling/roots/elicitation (answered -32601), `list_changed` (N2), resources/prompts |

## Live drive (real model in Herdr panes, `deepseek/deepseek-v4.1-flash-1017` via OpenRouter; script `scripts/live/m6_live.sh`)

| Run | Observed | Verdict |
|---|---|---|
| A (`live-m6a`, test server) | `kelir mcp ls`, `demo ls`, `demo echo text=hello`, `demo structured \| jq .answer` -> `42`. Log: `tool_started -> mcp_started (parent_call_id = the bash call id) -> mcp_settled (ok, exit 0, out_path under commands/mcp_*) -> tool_settled`. (A first attempt failed because my script wrote a POSIX path into the toml; the model spent 45 steps. That exposed F3 and N1. Script fixed, re-run clean.) | PASS |
| B (`live-m6b`) | bare `kelir mcp demo big`: `tool_settled` = `[output spilled: 20020 bytes, 308 lines; full output at .../commands/6-1-1/out]`; `\| wc -c` = 20020; `mcp_settled.bytes` 20020, the file holds the whole result | PASS |
| C (`live-m6c`, real `server-everything`) | `ls --detail name` (13 tools), `get-sum a=40 b=2` -> "The sum of 40 and 2 is 42."; `mcp_started`/`mcp_settled` ok | PASS |

Real-server one-shot drives (no model): echo with unicode, `get-sum` with a bad type (exit 2, names the path and value),
missing required (exit 2), `--schema`, structuredContent (compact JSON), `get-tiny-image` (PNG written, path printed),
`get-resource-links`, a long-running op with progress, unknown tool (exit 2, lists tools), `--json`, `--envelope`, filesystem
`list_directory`/`read_text_file`, and an out-of-roots read (server `isError` -> exit 1, text on stderr). All correct.

Break-it drives: 50 MB result (ok), hung `initialize` (exit 3 at 60 s), session killed mid-call (server gone; F8 on resume),
two concurrent calls (both logged with distinct `mcp_id` and the same parent; the stdlib test server is single-threaded so
the wall time is serial, kelir does not serialise), junk/CRLF/split writes (ok), 12 MB (ok). Side note: my own probe of
`get-env` printed the ambient provider key into my terminal output; it was not written to any file.

## Acceptance lines of `docs/tasks/M6.md`

| Line | Status |
|---|---|
| mcp.toml layering | done, tested (unit + e2e); F1, N1 |
| CLI shapes, `--detail`, `--schema`, `--envelope`, `--json -` | done |
| Exit codes 0/1/2/3/4, 4 never retried | done (e2e); F5 is the one path that could retry |
| Broker, lazy start, `parent_call_id`, spill | done; F8 |
| Channel servers at start, `source kind=channel` | done (e2e), queued out of tool batches |
| One-shot outside a session | done |
| Hand-rolled instead of `rmcp` | documented in DEVIATIONS/DEPS, judged sound |
| Hint / `observe` listing of servers (item 5) | **not done** (neither exists on this base); `Broker::descriptions()` is the hook. Not a blocker |
| STEER-7 token | not on base; list above |
| `docs/reference` updated | done (`mcp.md`, cli/env/events/files/verbs); drift test passes |

## Verdict

PASS WITH FIXES. Fix F1 (blocker) and F2-F8 before merge (or file them as steering items), then re-run the gate and
`cargo nextest run --test review_m6 --run-ignored all` (all 18 must pass; then remove the `ignore` attributes).
