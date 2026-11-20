# MCP: `mcp.toml` and `kelir mcp`

**Summary:** how MCP servers are configured (`mcp.toml`, two layers) and used
(`kelir mcp` from a bash call): the commands, the exit codes, the broker in the
session process, what is logged, channel servers.
**Read this when:** you are adding or debugging an MCP server, reading
`mcp_started`/`mcp_settled` in a log, or wondering why a call came back with
exit 3 or 4 (`src/mcp/`: `config.rs`, `rpc.rs`, `broker.rs`, `cli.rs`).

The model gets no MCP tool schemas in its request. It runs a CLI from `bash`,
reads the schema only when it needs it, and pipes the result like any other
command: `kelir mcp notes search q=invoice | jq '.hits[0]'`.

## `mcp.toml`

Two layers, machine then project, merged **by server name** (a project's
`[servers.x]` replaces the machine's whole, never field by field):

| layer | file |
|---|---|
| machine | `<config dir>/mcp.toml`; `KELIR_MCP_CONFIG` replaces the path |
| project | the nearest `.kelir/mcp.toml`, walking up from the working directory |

```toml
[servers.notes]
command = "npx"                         # required; a bare Windows name finds .exe, .cmd, .bat
args = ["-y", "some-mcp-server"]
env = { NOTES_DIR = "C:/notes" }        # extra environment for the server; never printed
description = "my notes"                # first line only; shown by `kelir mcp ls`
channel = false                         # true: connected when the session starts
timeout = 60                            # seconds one call may take (default 60; 0 is refused)
```

A server name is letters, digits, `-` and `_`, and not `ls`. Unknown keys are
an error that names the file (a typo must not silently do nothing). Only the
stdio transport exists. A bad file does not stop a session: `kelir mcp` prints
the error and exits 2.

- **Errors never quote the file.** A syntax or type error prints the file, the
  line and column and the parser's message, never the source line or the
  offending value: `env` holds secrets, and the error text reaches the model
  and the event log.
- **The server's environment is an allow-list.** A server inherits `PATH`,
  `HOME`/`USERPROFILE`, `TEMP`/`TMP`/`TMPDIR`, `SystemRoot` and the other
  variables a process needs to start, locale (`LANG`, `LC_*`), proxy and CA
  variables (`HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY`, `SSL_CERT_FILE`,
  `NODE_EXTRA_CA_CERTS`) and nothing else: not `KELIR_*`, not provider keys.
  Anything more it needs goes in its `env` table.
- **When the file is read.** The session reads both layers once, when it
  starts. Editing `mcp.toml` has no effect on a running session (`kelir resume`
  or a new session picks it up); a one-shot `kelir mcp` outside a session
  reads it on every command.
- **A project file is code.** `.kelir/mcp.toml` can name any `command`, and
  `channel = true` starts it when the session starts. Only open projects you
  trust; a trust prompt for project-defined servers belongs with the verb
  authentication work (STEER-7).

## The commands

```text
kelir mcp ls [--json]                              servers: name, state, tool count, description
kelir mcp <server> ls [--detail name|desc|full]    tools; full = the whole tool objects, inputSchema included
kelir mcp <server> <tool> [k=v …] [--json '{…}'] [--schema] [--envelope]
```

- `k=v` is typed from the tool's `inputSchema`: a property that accepts a
  string (`type: "string"`, `["string","null"]`, or an `anyOf`/`oneOf` branch
  that is a string) and no other scalar keeps the text as typed (`id=007`,
  `id=123`); anything else is JSON when it parses (`n=3`, `ok=true`,
  `tags=["a"]`) and a string when it does not. An integer too big for 64 bits
  (`id=123456789012345678901234567890`) stays text instead of being rounded to
  a float. A property that accepts a string and a number reads `123` as the
  number and `abc` as the text.
- A tool named `ls` (or `call`) is reached as `kelir mcp <server> call <tool> …`. `--json`
  gives an object, `--json -` reads it from stdin; `k=v` pairs win over it.
- The arguments are checked against the schema (`type`, `enum`, `required`,
  `properties`, `additionalProperties: false`, `items`, `minimum`/`maximum`,
  `anyOf`/`oneOf`)
  **before** the server is asked; every problem is listed, exit 2.
- `--schema` prints the tool's `inputSchema` and calls nothing.
- stdout is `structuredContent` as compact JSON when the tool sent it, else the
  text blocks joined by newlines. `--envelope` prints the whole result object.
- Images, audio and binary resources are decoded to files (`commands/mcp_…/` in
  a session, `<state>/mcp/<id>/` outside one) and the paths are printed — on
  stdout after the text, or on stderr when stdout is JSON that a pipe will parse.
- A tool that answers `isError` prints its text on **stderr** and exits 1, so
  `| jq` never reads an error as data.

| exit | meaning |
|---|---|
| 0 | ok |
| 1 | the tool reported `isError` (or the server answered with a JSON-RPC error) |
| 2 | usage: bad words, unknown server or tool, arguments that fail the schema, bad `mcp.toml` |
| 3 | server unavailable: it would not start, did not initialize, or died during the call |
| 4 | timeout: no answer within the server's `timeout`. The outcome is **unknown** and the call is **never retried** |

## The broker

Inside a session `KELIR_SOCK` is set, and `kelir mcp` sends every request to the
**session process** (the broker) instead of starting anything itself:

- A server is started by the first call that needs it, kept for the session and
  restarted by the next call after it dies. Each server is a process tree of its
  own (a process group on Unix, a kill-on-close Job Object on Windows): ending
  the session, or killing it outright, takes the servers with it.
- Calls to one server run concurrently; starting a server is serialised.
- A request that fails after it reached the session (the session died or
  closed the socket mid-call) is exit 4, not a one-shot retry: the tool may
  have run. Only a failed *connect* falls back to starting the server in the
  CLI. A session that is killed mid-call and resumed gets `mcp_settled` with
  `outcome: "unknown"` (exit 4) for the call, and the call is not re-run.
- A server that dies while starting is exit 3 with the last line of its
  stderr in the message (`(server stderr: …)`), the one place it says why.
- `notifications/tools/list_changed` makes the broker read the tool list again
  at the next request.
- Each call is an `mcp_started` / `mcp_settled` pair in the log, written by the
  session (the only writer). `parent_call_id` is the bash call it was made from,
  so a log reads `tool_started → mcp_started → mcp_settled → tool_settled`.
  `mcp_settled.out_path` is the file holding the whole result (a result over
  10 KB is never inline in the log); the CLI still prints the whole result, so a
  pipe sees every byte, and the bash call's own 10 KB rule gives the model a
  spill notice when the output is not piped.
- The broker's internal verbs (`mcp-settle`, `mcp-notify`) carry a per-session
  random key, so a socket client cannot forge a settlement.
- **Verb authentication (STEER-7).** Every request the CLI sends is the one
  verb `mcp` (`op` = `servers`, `tools` or `call`), built and sent in one place
  (`Backend::ask` in `cli.rs`, served by `Session::mcp_verb`). When the session
  token lands, all three ops must carry it like `view` and `bg`: `call` runs a
  tool, and `servers`/`tools` start servers. `mcp-settle` and `mcp-notify`
  travel in-process, not through the listener, and keep their key as the
  second layer. The token variable must also stay out of the servers'
  environment (it is not on the allow-list).
- `kelir ctl <session> tools '{"disable":"mcp:<server>.<tool>"}'` (or
  `mcp:<server>`) switches calls off: they are logged and answered
  `disabled by user` (exit 1).
- Stopping the bash call kills the CLI, not the in-flight server request; the
  call still settles in the log when the server answers or times out.

Outside a session (a plain shell, or a background job that outlived its
session and cannot reach the socket) the CLI starts the server for that one
command and kills its tree when the command ends.

## Channel servers

A server with `channel = true` is connected when the session starts, because
inbound messages arrive on that connection. A `notifications/claude/channel`
notification (`params.content`, optional `params.meta`) is written as a
`source` event with `kind: "channel"` and `name` the server, at the next safe
boundary (never between a tool call and its result). It is context, not a wake:
the policy that acts on a message is a later milestone. A channel server that
cannot connect is an `error` event (`where: mcp`).

## Footprint

No `rmcp`: the client is a hand-rolled JSON-RPC subset (`initialize`,
`tools/list`, `tools/call`, `notifications/cancelled`, `ping`, inbound
notifications), roughly 400 lines and no new dependency. See
`docs/DEVIATIONS.md`.
