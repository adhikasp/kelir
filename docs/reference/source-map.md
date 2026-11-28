# Source map

**Summary:** one line per source file: what it owns, so the next question is a
`sed -n` on one file rather than a walk of the tree.
**Read this when:** the reference's pointers (`src/config.rs:load`) have taken you
as far as prose can, and you are changing the code.

The tree is organised by subject. Each directory owns one concern; `core/` is
the loop, and `features/` plug into its boundary trait. This map is generated
from the file list (`bash scripts/source-map-check.sh`) and that check fails if
a file is missing or an entry is stale.

## Top level

| file | owns |
|---|---|
| `src/lib.rs` | crate root: the allowed `#![feature]` list and the top-level module declarations |
| `src/main.rs` | the `kelir` entry point: clap surface and dispatch |
| `src/main_cli.rs` | `run` (with `--ask`, `--resume`, `--name`, `--single-flight`, `--timeout`, `--fresh`) and the CLI helpers |
| `src/config/mod.rs` | `config.toml`: machine config, then the project's `.kelir/config.toml` |
| `src/config/load.rs` | config discovery, file loading and merge |
| `src/config/raw.rs` | raw TOML shapes, defaults and provider constants |
| `src/config/resolve.rs` | resolution of raw config into typed runtime values |
| `src/config/types.rs` | public config types and the accepted-key list |
| `src/paths.rs` | per-OS state and config directories |
| `src/util.rs` | small shared helpers: hashing, truncation, redaction, path strings |
| `src/envs.rs` | every environment variable kelir reads or sets, in one table |

## `src/core/` — the loop; knows no feature by name

| file | owns |
|---|---|
| `src/core/mod.rs` | the core module: the loop's boundaries and their order |
| `src/core/boundary.rs` | the `Feature` trait: how a feature reaches the core without being named by it |
| `src/core/session.rs` | the session: fold → request → stream → log → tools → boundaries → repeat |
| `src/core/session/boot.rs` | process options, the main run loop and preparation |
| `src/core/session/lifecycle.rs` | session construction and log reopen |
| `src/core/session/mainloop.rs` | the outer run/idle loop and its boundary helpers |
| `src/core/session/toolloop.rs` | the tool half: run a batch, settle a call, kill a tree |
| `src/core/session/turnloop.rs` | the turn half: drive one attempt and its retries |
| `src/core/session/verbloop.rs` | the verb half: accept, parse, log and apply verbs |
| `src/core/session/compactloop.rs` | the compaction half of the loop |
| `src/core/session/model.rs` | model capabilities and request shaping |
| `src/core/session/report.rs` | turn reporting and final state answers |
| `src/core/session/view.rs` | message folding, attachment import and view checks |
| `src/core/session/types.rs` | `Session` (at most 15 fields) and its local types |
| `src/core/session/util.rs` | free helpers for result events, prompt rendering and task input |
| `src/core/session/tests.rs` | crate-visible helpers shared by the loop's unit tests |
| `src/core/verbs.rs` | verb parsing, the scheduler and their application at boundaries |
| `src/core/compact.rs` | replacing a prefix of the conversation with a summary |

## `src/log/` — the event log, the only source of truth

| file | owns |
|---|---|
| `src/log/mod.rs` | the log module: event v1, fold, resume and spill |
| `src/log/event/mod.rs` | the event log and its append contract |
| `src/log/event/log.rs` | the append-only JSONL writer and reader |
| `src/log/event/types.rs` | event bodies: usage, calls, notes and the event enum |
| `src/log/event/payload.rs` | payload representation, blobs and call helpers |
| `src/log/event/spill.rs` | large-payload spilling to the blob directory |
| `src/log/fold.rs` | `fold(events.jsonl)`: the one path from log to context |
| `src/log/resume.rs` | continuing a session from its log |
| `src/log/spill.rs` | the head/tail excerpt shown when an output is too big |

## `src/provider/` — requests, streams, keys and governor

| file | owns |
|---|---|
| `src/provider/mod.rs` | one OpenAI Chat Completions client for the configured providers |
| `src/provider/chat.rs` | chat messages in the Chat Completions shape |
| `src/provider/sse.rs` | the incremental SSE parser (bytes to events) |
| `src/provider/stream.rs` | assembling a stream into one assistant turn |
| `src/provider/retry.rs` | what to retry, backoff and the provider's own `Retry-After` |
| `src/provider/models.rs` | model metadata fetched from the provider, not a table in source |
| `src/provider/keys.rs` | key sources and their precedence |
| `src/provider/governor.rs` | the client-side per-provider concurrency cap |
| `src/provider/usage.rs` | the token tally and the status it answers |

## `src/sys/` — the one runner and the OS primitives

| file | owns |
|---|---|
| `src/sys/mod.rs` | the process boundary: the only way `src/` starts a process |
| `src/sys/runner.rs` | the one child runner (the only `Command::new` in `src/`) |
| `src/sys/shell.rs` | how a command reaches the shell: as an argument or a script file |
| `src/sys/signals.rs` | Ctrl-C / SIGTERM as the session's own `stop` |
| `src/sys/proc/mod.rs` | tool-child process grouping, so a stop kills the whole tree |
| `src/sys/proc/core.rs` | cross-platform group helpers and Unix process-tree probes |
| `src/sys/proc/win.rs` | the Windows Job Object shim and handle helpers |
| `src/sys/atomic.rs` | small file primitives shared by names, locks and STATUS |
| `src/sys/joblock.rs` | single-flight: one lock per named job |
| `src/sys/perm.rs` | owner-only creation of the socket, pipe and token file |
| `src/sys/trust.rs` | the session socket's token and the jobs it owns |

## `src/features/` — one directory per feature

| file | owns |
|---|---|
| `src/features/mod.rs` | the feature registry: one `Features`, hooks called by boundary name |
| `src/features/attachments.rs` | pasted images as session files plus a pointer in the prompt |
| `src/features/memory.rs` | a configured notes directory, loaded as sources |
| `src/features/models.rs` | the models/window feature: metadata, window size, model swaps |
| `src/features/notify.rs` | `[notify]` hooks and event notifications |
| `src/features/bg/mod.rs` | background jobs: shim, status files and `bg` verbs |
| `src/features/bg/shim.rs` | the detached bg process itself |
| `src/features/bg/cli.rs` | the `kelir bg` command line |
| `src/features/bg/launch.rs` | launching a bg job |
| `src/features/bg/status.rs` | reading and writing bg status files |
| `src/features/bg/text.rs` | bg job text and notices |
| `src/features/channel/mod.rs` | channel mode: chat bridges through MCP |
| `src/features/channel/driver.rs` | the channel driver and its durable spool |
| `src/features/channel/message.rs` | channel message shapes |
| `src/features/channel/run.rs` | running a channel round trip |
| `src/features/children/mod.rs` | children: registry, caps and the hand-off to a child process |
| `src/features/children/feature.rs` | the session side of the `child` verbs |
| `src/features/children/cli.rs` | `kelir run --child`, `kelir brief`, `kelir kill`, `kelir ls` |
| `src/features/children/ops.rs` | root registry operations and child settling |
| `src/features/gates/mod.rs` | gates: the only way a veto or nudge enters the core |
| `src/features/gates/feature.rs` | the gates feature: builds the runner from config and skills |
| `src/features/gates/config.rs` | gate declarations and `[gates]` configuration |
| `src/features/gates/decision.rs` | gate decision parsing |
| `src/features/gates/runner.rs` | running external gates at their boundaries |
| `src/features/gates/limits.rs` | gate timeouts, process caps and the token sweep |
| `src/features/gates/builtin/mod.rs` | built-in gates behind the external gate interface |
| `src/features/gates/builtin/hint.rs` | `builtin = "hint"`: "might be relevant: name - description" |
| `src/features/gates/builtin/timebox.rs` | `builtin = "timebox"`: a turn with a clock does not end before it runs out |
| `src/features/herdr/mod.rs` | herdr agent status: one OSC 7501 record per change |
| `src/features/herdr/ctx.rs` | the one frozen herdr line in the system sources |
| `src/features/mcp/mod.rs` | MCP as a CLI |
| `src/features/mcp/feature.rs` | the MCP feature: broker lifecycle and internal verbs |
| `src/features/mcp/broker.rs` | the session-side MCP broker |
| `src/features/mcp/cli.rs` | the `kelir mcp` client side |
| `src/features/mcp/config.rs` | `[mcp.servers.<name>]` parsing |
| `src/features/mcp/args.rs` | `k=v` arguments and schema checks |
| `src/features/mcp/rpc.rs` | the small stdio JSON-RPC client |
| `src/features/mcp/render.rs` | a `tools/call` result as CLI text and exit code |
| `src/features/names/mod.rs` | named sessions: `names/<name>` to a session id |
| `src/features/names/namedrun.rs` | `run --name --single-flight`: scheduler flags |
| `src/features/sources/mod.rs` | sources: instruction files, skills and memory in the prompt |
| `src/features/sources/feature.rs` | the sources feature and its on-disk watcher |
| `src/features/sources/source/mod.rs` | the source tree: base prompt, instructions, skills |
| `src/features/sources/source/discover.rs` | discovering source files and skills directories |
| `src/features/sources/source/render.rs` | rendering sources into the frozen prompt |
| `src/features/sources/source/types.rs` | source types and their order |
| `src/features/sources/source/watch.rs` | watching source files for changes |

## `src/ipc/` — driving a live session

| file | owns |
|---|---|
| `src/ipc/mod.rs` | the session socket: newline-delimited JSON (a named pipe on Windows) |
| `src/ipc/ctl.rs` | `kelir ctl`: send one verb to a running session |
| `src/ipc/attach.rs` | replay the event log through the TUI's row model |
| `src/ipc/tail.rs` | bounded tail reader for `kelir attach` |
| `src/ipc/observe.rs` | `kelir observe`: the next request, human- or `--raw`-readable |
| `src/ipc/doctor.rs` | `kelir doctor`: provider, key and path diagnosis |

## `src/tools/` — the native tool calls

| file | owns |
|---|---|
| `src/tools/mod.rs` | the tool list, JSON schemas, dispatch and disable/enable |
| `src/tools/bash.rs` | the `bash` call: shell, output tee, promotion |
| `src/tools/edit.rs` | the `edit` call: exact replacement, line endings, create |

## `src/tui/` — the full-screen face

| file | owns |
|---|---|
| `src/tui/mod.rs` | the TUI module: alternate screen, transcript viewport |
| `src/tui/app/mod.rs` | the app state: every row comes from the events |
| `src/tui/app/entry.rs` | app methods: entry |
| `src/tui/app/events.rs` | app methods: events |
| `src/tui/app/input.rs` | app methods: input |
| `src/tui/app/layout.rs` | app methods: layout |
| `src/tui/view.rs` | status/format helpers, the layout and painting |
| `src/tui/keys.rs` | keyboard translation and the kitty keyboard handshake |
| `src/tui/commands.rs` | `/` commands: one line in, one verb out |
| `src/tui/complete.rs` | the `/` popup: catalogue, score, context |
| `src/tui/editor.rs` | the input editor: a buffer, cursor and history |
| `src/tui/editor_impl.rs` | the editor's line and cursor helpers (split for the 800-line cap) |
| `src/tui/terminal.rs` | terminal setup, teardown and restore |
| `src/tui/activity.rs` | the state row's activity machine |
| `src/tui/theme.rs` | every style and glyph, with `NO_COLOR` and ASCII fallbacks |
| `src/tui/markdown/mod.rs` | the agent's markdown renderer |
| `src/tui/markdown/render.rs` | markdown blocks to styled rows |
| `src/tui/markdown/stream.rs` | incremental markdown rendering during a stream |
| `src/tui/markdown/table.rs` | markdown tables |
| `src/tui/markdown/wrap.rs` | wrapping and display width |
| `src/tui/scrollback.rs` | text into terminal rows |
| `src/tui/tools_view.rs` | one dim line per tool call and its append-only expansion |

## `src/docs/` — the embedded reference

| file | owns |
|---|---|
| `src/docs/mod.rs` | `kelir docs`: topics, `--list`, `--search`, the embedded text |
| `src/docs/tests.rs` | the drift test: code names against `docs/reference/` |

## Around the code

| path | owns |
|---|---|
| `docs/reference/` | the reference itself (these files) |
| `tests/support/` | the mock provider and the binary harness every e2e test uses |
| `tests/it/` | integration/e2e tests, filed by feature |
| `scripts/gate.sh` | the gate: shape, fmt, clippy, nextest, doc tests, optionally Linux |
| `scripts/shape.sh` | the shape rules; `--self-test` seeds a violation for each rule |
| `scripts/source-map-check.sh` | the drift check: one row per `src/` file in this map |
