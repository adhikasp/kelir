# Source map

**Summary:** one line per source file: what it owns, so the next question is a
`sed -n` on one file rather than a walk of the tree.
**Read this when:** the reference's pointers (`src/config.rs:load`) have taken you
as far as prose can, and you are changing the code.

The tree is flat on purpose: a file is a subject.

## The loop

| file | owns |
|---|---|
| `src/session.rs` | the session process: prepare, the step loop, boundaries, tool batches, resume, `run`/`ask`/`tui`/`view` |
| `src/fold.rs` | `fold(events.jsonl)`: the messages a request is built from |
| `src/stream.rs` | one provider response: SSE in, deltas and tool-call fragments out |
| `src/sse.rs` | the SSE parser (bytes to events, no HTTP) |
| `src/provider.rs` | the request: headers, body, cache control, `parallel_tool_calls` |
| `src/retry.rs` | backoff, budgets, the provider's own `Retry-After` |
| `src/models.rs` | `GET /models`: one normalised list, cached per endpoint |
| `src/governor.rs` | the advisory-lock concurrency limit per provider |

## The log

| file | owns |
|---|---|
| `src/event.rs` | `EventBody`, `LogEvent`, the writer, spill to `blobs/`, torn-tail repair |
| `src/resume.rs` | reopening a log: cwd, model, unsettled calls |

## Steering

| file | owns |
|---|---|
| `src/verbs.rs` | `Verb`, `parse`, `Boundary`, `Scheduler` |
| `src/ctl.rs` | `kelir ctl`: find a session, send one verb, print the answer |
| `src/ipc.rs` | the socket: verbs in, status out, one client at a time |
| `src/commands.rs` | the TUI's `/` commands |
| `src/signals.rs` | Ctrl-C, `SIGTERM`, and the "stop the tree, not the world" rules |

## Context in

| file | owns |
|---|---|
| `src/sources.rs` | prompt, instruction files, skills discovery, the per-boundary watcher |
| `src/attachments.rs` | `kelir view`: sniff, copy, the `image_url` part |
| `src/compact.rs` | the summary prompt and the compaction fold |
| `src/prompt.md` | the base prompt itself (1 KB, snapshot-pinned) |

## Tools and jobs

| file | owns |
|---|---|
| `src/tools/mod.rs` | the tool list, the JSON schemas, dispatch, disable/enable |
| `src/tools/bash.rs` | the `bash` call: the shell, the output tee, promotion |
| `src/tools/edit.rs` | `edit`: multi-hunk replace, line endings, the create case |
| `src/shellcmd.rs` | building the command line (Windows quoting, script files, CRLF) |
| `src/spill.rs` | the head/tail excerpt and the spill notice |
| `src/proc.rs` | process trees: Job Objects on Windows, process groups on Unix |
| `src/bg.rs` | background jobs: the shim, status files, `bg` verbs, settle |
| `src/mcp/` | `kelir mcp`: `config.rs` (`mcp.toml`), `rpc.rs` (stdio JSON-RPC client), `args.rs` (words, `k=v`, schema check), `render.rs` (result, exit codes), `broker.rs` (session side), `cli.rs` (client side) |

## The face

| file | owns |
|---|---|
| `src/tui.rs` | the interactive session: rows, keys, the loop, the terminal restore |
| `src/activity.rs` | the state row's machine (`idle`, `thinking`, `running`, `rate limited`, …) |
| `src/theme.rs` | colours and glyphs (`NO_COLOR`, `KELIR_ASCII`, `TERM=dumb`) |
| `src/markdown.rs` | the renderer |
| `src/tools_view.rs` | one line per tool call, groups, collapsed/expanded |
| `src/editor.rs` | the input buffer and its image chips |
| `src/scrollback.rs` | finished turns written above the live area |
| `src/herdr.rs` | the OSC 7501 status record |

## Self-documentation

| file | owns |
|---|---|
| `src/docs/mod.rs` | `kelir docs`: topics, search, the generated blocks, `--check`/`--generate` |
| `src/docs/tables.rs` | the verbs, config keys and event types the reference is generated from |
| `src/docs/skill-kelir.md` | the builtin `kelir` skill |
| `src/envs.rs` | every environment variable kelir reads or sets |
| `docs/reference/` | the reference itself (these files) |

## Around the code

| path | owns |
|---|---|
| `src/config.rs` | `config.toml`: every key, the merge, the shell, the retry policy |
| `src/credentials.rs` | key sources and their precedence, opencode's store |
| `src/paths.rs` | state/config/home per OS |
| `src/cli.rs`, `src/main.rs` | the clap surface and the dispatch |
| `src/doctor.rs` | `kelir doctor` |
| `src/util.rs` | hashing, truncation, redaction, path strings |
| `tests/support/mod.rs` | the mock provider and the binary harness every e2e test uses |
| `scripts/gate.sh` | the gate: fmt, clippy, nextest, doc tests, optionally Linux |
