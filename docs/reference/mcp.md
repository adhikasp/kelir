# MCP: `config.toml` and `kelir mcp`

**Summary:** how MCP servers are configured (`config.toml`, two layers) and used
(`kelir mcp` from a bash call): the commands, the exit codes, the broker in the
session process, what is logged, channel servers.
**Read this when:** you are adding or debugging an MCP server, reading
`call_started{kind:"mcp"}` / `call_settled{kind:"mcp"}` in a log, or wondering why a call came back with
exit 3 or 4 (`src/mcp/`: `config.rs`, `rpc.rs`, `broker.rs`, `cli.rs`).

The model gets no MCP tool schemas in its request. It runs a CLI from `bash`,
reads the schema only when it needs it, and pipes the result like any other
command: `kelir mcp notes search q=invoice | jq '.hits[0]'`.

## `config.toml`

Two layers, machine then project, merged **by server name** (a project's
`[mcp.servers.x]` replaces the machine's whole, never field by field):

| layer | file |
|---|---|
| machine | `<config dir>/config.toml`; `KELIR_CONFIG` replaces the path |
| project | the nearest `.kelir/config.toml`, walking up from the working directory |

```toml
[mcp.servers.notes]
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
  Anything more it needs goes in its `env` table. That includes registry and
  tool settings that exist only in your environment (`npm_config_*`, `UV_*`,
  `PYTHONPATH`, `VIRTUAL_ENV`, `NODE_OPTIONS`): put them in `env`.
- **A server's stderr is redacted.** The last line of a failed server's stderr
  is shown in errors and `kelir mcp ls`; every `env` value of 6 or more bytes
  in it is replaced by `***`.
- **A killed session leaves no server.** Each server runs under a small watcher
  in its process group that kills the group when the session is gone, even if
  the server ignores stdin EOF (a Job Object on Windows).
- **When the file is read.** Both layers, once, when the session starts: editing
  `config.toml` does not reach a running session (a `--resume` or a new one picks
  it up). A one-shot `kelir mcp` reads it on every command.
- **A project file is code.** `.kelir/config.toml` can name any `command`, and
  `channel = true` starts it with the session. Only open projects you trust.

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
- Images, audio and binary resources are decoded to files (`calls/mcp_…/` in
  a session, `<state>/calls/mcp_<id>/` outside one) and the paths are printed — on
  stdout after the text, or on stderr when stdout is JSON that a pipe will parse.
- A tool that answers `isError` prints its text on **stderr** and exits 1, so
  `| jq` never reads an error as data.

| exit | meaning |
|---|---|
| 0 | ok |
| 1 | the tool reported `isError` (or the server answered with a JSON-RPC error) |
| 2 | usage: bad words, unknown server or tool, arguments that fail the schema, bad `config.toml` |
| 3 | server unavailable: it would not start, did not initialize, or died during the call |
| 4 | timeout: no answer within the server's `timeout`. The outcome is **unknown** and the call is **never retried** |

## The broker

Inside a session `KELIR_SOCK` is set, and `kelir mcp` sends every request to the
**session process** (the broker) instead of starting anything itself:

- A server starts on the first call that needs it, is kept for the session and
  restarted by the next call after it dies. Its process tree is its own (group on
  Unix, kill-on-close Job Object on Windows), so ending the session takes it.
- Calls to one server run concurrently; starting one is serialised.
- A request that failed after reaching the session is exit 4, never a retried
  one-shot: the tool may have run. Only a failed *connect* falls back to starting
  the server in the CLI. A session killed mid-call and resumed gets
  `call_settled` `kind: "mcp"`, `outcome: "unknown"`, and does not re-run it.
- A server that dies while starting is exit 3, with the last line of its stderr
  in the message (`(server stderr: …)`), the one place it says why.
- `notifications/tools/list_changed` makes the broker re-read the tool list at
  the next request.
- Each call is a `call_started`/`call_settled` pair written by the session (the
  only writer), `kind: "mcp"`, `info.name = "<server>/<tool>"`. `out_path` holds
  the whole result (over 10 KB is never inline); the CLI prints every byte, so a
  pipe sees it all, and the bash call's own 10 KB rule spills the rest.
- The broker's internal verbs carry a per-session random key, so a socket client
  cannot forge a settlement.
- **Verb authentication.** Every request the CLI sends is the one verb `mcp`
  (`op` = `servers`, `tools` or `call`), built in one place (`Backend::ask` in
  `cli.rs`, served by `Session::mcp_verb`), and all three carry the session
  token from `KELIR_TOKEN` exactly as `kelir bg` does. The listener refuses a
  request without it before anything is logged or any server starts, and the CLI
  reports exit 3 rather than falling back to an unlogged one-shot call. Outside
  a session there is no token and none is needed; `mcp-settle`/`mcp-notify`
  travel in-process and keep their key as the second layer.
- **Secrets.** A server's `env` table goes to its child only: never printed,
  logged or echoed in an error. `call_started.info.input` records the *shape* of
  the arguments (keys kept, strings as `<string, N bytes>`) and the values live in
  the enclosing bash call. The server's environment is an allow-list (no
  `KELIR_*`, so no token or socket) and no secret reaches its command line.
- `kelir ctl <session> tools '{"disable":"mcp:<server>.<tool>"}'` (or
  `mcp:<server>`) switches calls off: they are logged and answered
  `disabled by user` (exit 1).
- Stopping the bash call kills the CLI, not the in-flight request: the call still
  settles in the log when the server answers or times out.

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
