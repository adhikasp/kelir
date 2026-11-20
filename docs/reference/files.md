# Files: state, config, session dirs

**Summary:** where kelir keeps everything on disk — the state root, a session's
directory and event log, spilled payloads, tool output, background jobs,
attachments, locks, model caches — per OS, plus the config search order and the
environment variables that move each of them.
**Read this when:** you are reading a session's log by hand, cleaning a state
dir up, or writing a test that must not touch the machine's own files
(`src/paths.rs`, `src/event.rs`, `src/bg.rs`, `src/models.rs`, `src/governor.rs`,
`src/ipc.rs`).

## Roots, per OS

| | Linux | macOS | Windows |
|---|---|---|---|
| state | `$XDG_STATE_HOME/kelir`, else `~/.local/state/kelir` | `~/Library/Application Support/kelir` | `%LOCALAPPDATA%\kelir` |
| config | `$XDG_CONFIG_HOME/kelir`, else `~/.config/kelir` | `~/Library/Application Support/kelir` | `%APPDATA%\kelir` |

`KELIR_STATE_DIR` and `KELIR_CONFIG_DIR` replace them outright. `KELIR_HOME`
does not: it only replaces `$HOME`/`USERPROFILE` (user skills), and the platform
variables above still win when set. Isolate a run with the two `*_DIR` variables.

## A session directory

```text
<state>/sessions/<ses_ulid>/
  events.jsonl            # append-only, fsync'd per batch, monotonic seq: the only source of truth
  events.jsonl.torn       # the fragment of a torn last line, kept when it was cut off
  sock                    # the session socket (a pipe name on Windows)
  commands/<call-id>/out  # a tool call's whole output, from the first byte
  gates/g<n>.sh|.bat|.ps1 # the command text of gate number n, written for the shell (`gates`)
  commands/mcp_<ulid>/    # a `kelir mcp` call: `out` (its text) and any decoded image/blob files
  blobs/<seq>-<field>     # a payload over 10 KB, referenced from its event
  attachments/<n>-<name>  # an image `kelir view` queued
  bg/<bg-id>.status       # a job's state, written by its shim: survives kelir exiting
  bg/<bg-id>.out          # a job's output
  owner                   # {"pid","start"} of the process that owns the session (who holds a name)
  host.err                # a child session only: its progress lines and errors (its stdout is its answer)
```

Session ids are ULIDs, so the directory names sort into creation order and
`ctl latest` can pick the newest; `kelir ls` shows them as a tree (a child's
`session_started` names its `parent`). The id is also the provider's prompt-cache key
(a `brief` uses its parent's). A child is a session of its own and lives in the same
`sessions/` directory as its parent.

## State root, outside a session

```text
<state>/
  sessions/<id>/…         # above
  names/<name>            # a named session: {"session","created","updated"} (owner-only dir)
  jobs/<job>/             # lock, STATUS.json, STATUS.md, runs.jsonl, notes/ (`docs/reference/jobs.md`)
  models-<endpoint>.json  # one endpoint's model list, cached for a day, written temp-then-rename
  locks/<provider>-<slot> # the concurrency governor's advisory locks
  bg/<bg-id>.*            # jobs started outside a session (kelir bg run from a plain shell)
  mcp/<ulid>/             # decoded images/blobs of a one-shot `kelir mcp` call (outside a session)
```

`locks/` holds OS file locks, not pid files: a kelir that crashes leaks nothing
because the kernel releases the lock. `models-<endpoint>.json` is named after the
*endpoint* (provider name plus a hash of the normalised base URL), so a proxy or
a second gateway sharing a state dir never inherits another server's numbers.

On Unix the socket lives in the session dir, or in `$XDG_RUNTIME_DIR` when a
process has no session dir yet; it is created 0700, and so is every directory
kelir creates.

## Config search order

1. `--config <path>` or `KELIR_CONFIG`: exactly that file.
2. Otherwise `<config>/config.toml`, then the nearest `.kelir/config.toml`
   walking up from the cwd (the project's, merged per key over the machine's).

`mcp.toml` follows the same two layers, in its own file: `<config>/mcp.toml`
(or `KELIR_MCP_CONFIG`), then the nearest `.kelir/mcp.toml`, merged per server
name (`docs/reference/mcp.md`).

## The pieces, and the file that owns each

| path | owner | what it is |
|---|---|---|
| `events.jsonl` | `src/event.rs` | the log (`docs/reference/events.md`) |
| `blobs/` | `src/event.rs` | a payload too big to be inline |
| `commands/<call-id>/out` | `src/tools/bash.rs`, `src/spill.rs` | a tool call's whole output |
| `commands/mcp_<ulid>/` | `src/mcp/broker.rs`, `src/mcp/mod.rs` | a `kelir mcp` call's text and decoded blobs |
| `gates/` | `src/gates.rs` | the script each gate's command string is run from |
| `bg/` | `src/bg.rs` | background jobs (they outlive the session on purpose) |
| `host.err` | `src/child.rs` | a child session's stderr (written by its parent's spawn) |
| `attachments/` | `src/attachments.rs` | images `kelir view` queued |
| `sock` | `src/ipc.rs` | verbs in, status out |
| `models-*.json` | `src/models.rs` | the provider's model metadata |
| `locks/` | `src/governor.rs` | at most N requests in flight per provider |
| `names/` | `src/names.rs` | named sessions; the session `owner` file |
| `jobs/<job>/` | `src/jobrun.rs`, `src/jobs.rs`, `src/joblock.rs` | the job lock, STATUS, run history, notes |

## Cleaning up, safely

A finished session is a directory; delete it when you no longer need its log.
Do not delete a session's directory while a `kelir` owns it: the socket, the log
and a job's status files are all live. `kelir resume <id>` is how a stopped
session is picked up, and it is also what folds in a job that settled while
nothing was running — a job's `bg/<id>.status` is the only record until then.
