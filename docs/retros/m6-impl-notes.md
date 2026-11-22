# M6 implementer notes: MCP as a CLI

Job `m6`: brief `docs/tasks/M6.md`. Worktree `kelir-wt/m6`, branch `job/m6`.

## What landed

| piece | where |
|---|---|
| `config.toml` (machine `<config>/config.toml` or `KELIR_CONFIG`, project `.kelir/config.toml`, merged by server name, unknown keys refused) | `src/mcp/config.rs` |
| stdio JSON-RPC client: `initialize`, `tools/list` (cursor), `tools/call`, `notifications/cancelled`, `ping`, inbound notifications; server in its own process group / kill-on-close Job Object | `src/mcp/rpc.rs` |
| the command line, `k=v` typing from the schema, schema check (`type`, `enum`, `required`, `properties`, `additionalProperties:false`, `items`, `minimum`/`maximum`) | `src/mcp/args.rs` |
| result to stdout/stderr/files, base64 decode, exit codes | `src/mcp/render.rs` |
| the broker (session side): lazy start, per-server lock, restart after death, channel autostart | `src/mcp/broker.rs` |
| the CLI (client side): broker over `KELIR_SOCK`, one-shot fallback | `src/mcp/cli.rs` |
| session glue: `mcp_verb`, `mcp_request`, `mcp_settle`, `mcp_notify`, `flush_channel_queue` | `src/session.rs` (one `impl` block, two 4-line intercepts, three fields) |
| `mcp_started` / `mcp_settled` events | `src/event.rs`, `src/docs/tables.rs` |
| `tools` verb accepts `mcp:<server>` and `mcp:<server>.<tool>` | `src/verbs.rs` |
| `ipc::request_value_within` (a slow verb needs more than the 10 s answer wait) | `src/ipc.rs` |
| reference topic `mcp`, `cli`/`files`/`events`/`env`/`verbs` updates | `docs/reference/` |
| test server and 15 e2e tests | `tests/support/mcp_server.py`, `tests/e2e_m6.rs` |

## How a call travels

`kelir mcp demo echo text=hi` in a bash call: the CLI asks the broker for `tools` (starts the
server, caches the list), types and validates the arguments against the schema (exit 2 on a
problem, the server is never asked), then sends `call` over `KELIR_SOCK` with
`KELIR_CALL_ID` as `parent_call_id`. The session loop records `mcp_started`, parks the client's
reply in `mcp_pending`, and runs the call on a task. The task hands the result back through the
**same inbox the socket feeds** as an internal `mcp-settle` verb; the loop records `mcp_settled`,
syncs, and only then answers the client. So the log keeps one writer, no new `select!` arm was
needed in any of the wait loops, and the answer never outruns the log.

## Decisions worth a second look

- **Hand-rolled client, no `rmcp`.** About 400 lines on already-linked crates; reasons in
  `docs/DEVIATIONS.md` and `docs/DEPS.md`. HTTP/SSE transport is not built.
- **The CLI prints the whole result; the log holds a path.** `| jq` has to work on a 20 KB result.
  The model still gets the bash call's own spill notice when the output is not piped.
- **An `isError` text goes to stderr** (exit 1) so a pipe never reads an error as data.
- **Channel messages are queued while a tool batch is in flight** (`live_calls` non-empty) and
  written at the next `apply()`: a `source` event is rendered as a user message, and one between an
  assistant `tool_calls` message and its results would break the provider's pairing (the same
  reason bg wakes are deferred). Idle, they are written at once. The M10 policy (waking the model,
  `turn_done`) is not here.
- **Internal verbs carry a per-session random key** (`Broker::key`). STEER-7's session token is not
  on this branch's base; when it merges, the `mcp` verb needs the token on the same path as `view`
  and `bg`.
- **`KELIR_CONFIG`** is new (the tests need to give a session a server list without touching
  the real config dir).
- **Restart:** a server that died is started again by the next call that finds it dead; a call
  that was in flight when it died is exit 3 and is not retried (the result is unknown).
- **Timeout** sends `notifications/cancelled` and leaves the server running. A server stuck in the
  call keeps the slot busy; that is the server's problem, not retried by kelir.
- **Linux hard kill:** a server is in its own process group, so `kill -9` of the session leaves it
  to notice its stdin closing (a well-behaved stdio server exits on EOF; the test server does). No
  `PDEATHSIG`: it fires when the *spawning thread* dies, which on a tokio worker is the wrong
  contract. Windows is covered by the kill-on-close Job Object (tested: the server is gone after
  `taskkill /F` of the session).

## Not done / open

- **Hint and `observe` listing of servers** (brief item 5): neither exists on this base (M5-B).
  `Broker::descriptions()` is the hook.
- **STEER-7 token** on the `mcp` verb (see above).
- **HTTP/SSE transport** (a stretch in the brief).
- **Gates:** only the name `mcp:<server>.<tool>` is honoured, by the `tools` verb; there is no
  gate runner on this base to call.
- **`stop --call` of the enclosing bash call** kills the CLI but not the in-flight server
  request; the call settles in the log when the server answers or times out.
- `kelir mcp ls` in a session shows `-` as the tool count of a server that has not started (it
  never starts one).

## Gate

`bash scripts/gate.sh --linux=<machine>` at e62620e: fmt, clippy `-D warnings`, nextest 961 passed /
3 skipped, doctest, Linux half: `gate: PASS`. No leftover `kelir.exe` or `mcp_server.py` after the
runs (checked with `Get-CimInstance Win32_Process`).
