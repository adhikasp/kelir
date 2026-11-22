# CLI (subcommands and flags)

**Summary:** `kelir` with no subcommand is the interactive session; `run`,
`ctl`, `view`, `bg`, `ls`, `doctor` and `docs` are the others. The
agent reaches the CLIs below through `bash`, so none of them costs tool-schema
text.
**Read this when:** scripting kelir, or adding a subcommand (`src/cli.rs`,
`src/main.rs`).

Every command takes clap's own `--help`, which is the same table one screen
away. The table below is written out by hand; the drift test (`cargo test --lib
docs`) checks the names that matter. Exit codes: `0` success, `1` a refusal or
a failed run, `2` a usage error (a bad flag, an unreadable config, an unknown
provider).

## `kelir` (no subcommand), `kelir tui`

The interactive session. Flags: `--model`, `--provider`, `--cwd`, `--config`,
`--max-steps`, `--resume <ID>`. Keys and rows: `docs/reference/tui.md`. Without
a TTY it is a usage error (exit 2).

## `kelir run [TASK] [--ask] [--json] [--keep] [--resume ID]`

Run one task to completion. `--ask` prints only the final text; `--resume
<ID>` continues a session from its log without re-running tool calls.
`--json` streams the event
log as NDJSON on stdout; `--keep` stays alive between turns (accepting
`say`/`later`/`end`).

## `kelir ctl <SESSION> <VERB> [ARGS]`

Send one verb to a running session (see `docs/reference/verbs.md`).

## `kelir view <PATH>`

Queue an image for the model's next message. It sends the `view` verb over the
session socket the bash call already carries (`KELIR_SOCK`), so it only works
inside a kelir session; outside one it says so and exits 1.

- The path is resolved against the session dir first (so `attachments/1-x.png`,
  the pointer a prompt carries, is enough), then the working directory.
- The session copies the image into `<session>/attachments/` and records a
  `source` event with `kind: "attachment"` and the relative path. The request
  that follows carries it as one `image_url` part of the **last** message, so
  every earlier message (and the provider's cached prefix) is unchanged.
- It prints `image queued: appears in your next message` and exits 0. These are
  refused with a message and exit 1, and a refusal changes nothing (no `source`
  event, no request):
  - a missing file, or a directory;
  - a file over 10 MB;
  - an unknown image type;
  - **bytes that are not an image**: the extension is only a claim, so the
    signature and header are read (`png`, `jpg`, `gif`, `webp`). A text file
    named `fake.png` and an empty `empty.png` are refused here rather than
    poisoning the conversation with a provider 400 that repeats on every later
    request. The `data:` URL's media type is the *sniffed* format too, not the
    extension, so a GIF or a WebP saved as `x.png` is sent as what it is;
  - **a model the provider marks as text-only** (`capabilities.vision: false`,
    or OpenRouter's `architecture.input_modalities` without `image`). A model
    the provider says nothing about is tried: a missing flag is not a refusal.

The model does not have to look: that is the point of the pointer.

## `kelir bg run|ls|out|wait|cancel`

Background jobs (`docs/reference/verbs.md` has the events). A job outlives the
session on purpose: its shim writes `calls/<id>/<id>.status`, and the next `run --resume`
folds a job that settled meanwhile into the log.

## `kelir doctor [--json] [--provider <name>]`

Which key each provider would use, and where it comes from: the source, never
the secret. Exit 0 when every provider that matters has a usable key, 1 when the
session's own provider (or an expired credential) does not.

## `kelir docs [TOPIC] [--list] [--search <words>]`

The reference, embedded in this binary with `include_str!`, so it works in a
session with no checkout on disk. `--list` names the topics and their
`**Summary:**` lines; a bare `kelir docs` is the index; `kelir docs <topic>`
prints one file; `--search <words>` prints matching lines as
`topic:line: text`, best-matching topic first. `--skill <name>` prints a
builtin skill in full. Exit codes: `0` a read, `1` an unknown topic or a search
with no hit (the message suggests a topic).

The whole surface:

### `kelir run`

Run a task to completion; `--ask` prints the final text only

| flag | takes | what it does |
|---|---|---|
| `--model` | `<MODEL>` | Model id for this run (overrides the config) |
| `--provider` | `<PROVIDER>` | Provider name: `openrouter` or `opencode-go` (there are exactly two) |
| `--cwd` | `<CWD>` | Working directory for the session (defaults to the current directory) |
| `--config` | `<CONFIG>` | Config file to use instead of the machine/project search |
| `--max-steps` | `<N>` | Cap one turn at N steps, then pause it (overrides `max_steps`). There is no cap by default; `0` also means no cap |
| `<TASK>` | `<TASK>` | The task. Read from stdin when omitted |
| `--ask` | nothing | Ask one question and print only the final text |
| `--json` | nothing | Stream the event log as NDJSON on stdout |
| `--keep` | nothing | Stay alive after the turn ends, accepting `say`, `later` and `end` |
| `--resume` | `<ID>` | Continue a session from its log instead of starting one |
| `--child` | nothing | Start the task as a child of the session this tool call runs in (it needs `KELIR_SOCK`): the child's final text is this command's stdout |
| `--background` | nothing | With `--child`: return a handle at once; the child's completion wakes the parent at its next boundary |
| `--name` | `<NAME>` | Run in the named session: create it, or resume it and add the task as a new message (`a-z`, `0-9`, `.`, `_`, `-`; up to 64 characters) |
| `--single-flight` | nothing | Take the name's single-flight lock, write its STATUS, and skip with exit 75 when another run holds the lock. Needs `--name` |
| `--timeout` | `<D>` | Wall-clock deadline for the run (`90`, `30m`, `2h`, `1d`, `1w`): stop the turn at the deadline and exit 124. Needs `--single-flight` |
| `--task-file` | `<PATH>` | Read the task from a file instead of the command line |
| `--fresh` | nothing | Start a fresh session and rebind the name, carrying its last summary or answer into the new one (scheduler decides when: a weekly cron line) |

### `kelir tui`

Start the interactive session (the default with no subcommand)

| flag | takes | what it does |
|---|---|---|
| `--model` | `<MODEL>` | Model id for this run (overrides the config) |
| `--provider` | `<PROVIDER>` | Provider name: `openrouter` or `opencode-go` (there are exactly two) |
| `--cwd` | `<CWD>` | Working directory for the session (defaults to the current directory) |
| `--config` | `<CONFIG>` | Config file to use instead of the machine/project search |
| `--max-steps` | `<N>` | Cap one turn at N steps, then pause it (overrides `max_steps`). There is no cap by default; `0` also means no cap |
| `--resume` | `<ID>` | Continue a session from its log |

### `kelir attach`

Watch a running or finished session (read-only by default)

| flag | takes | what it does |
|---|---|---|
| `<SESSION>` | `<SESSION>` | Session id, a unique prefix, a name, or `latest` |
| `--follow` | nothing | Keep reading as the session writes new events |
| `--since` | `<N>` | Replay only the last N events (0 = all) |
| `--raw` | nothing | Print raw NDJSON instead of the TUI's row model |
| `--plain` | nothing | Print unstyled text for pipes |
| `--steer` | nothing | Allow `say <text>` and `stop` from the viewer (sent with the session's token) |
| `--config` | `<CONFIG>` | Config file to use instead of the machine/project search |

### `kelir ctl`

Send a verb to a running session and print its answer

| flag | takes | what it does |
|---|---|---|
| `<SESSION>` | `<SESSION>` | Session id, a unique prefix, or `latest` (the newest session only; an error if it is not running) |
| `<VERB>` | `<VERB>` | The verb: say, later, stop, model, tools, compact, skills, view, observe, cancel, status, brief, end |
| `<ARGS>` | `<ARGS>` | Text for `say`/`later`/`stop`, or one JSON object with the verb's args |
| `--now` | nothing | `say --now`: stop the running tool calls first |
| `--call` | `<CALL>` | `stop --call <id>`: stop one running call; the turn continues |
| `--reason` | `<REASON>` | Why: goes into the stopped call's result |
| `--disable` | `<TOOL>` | `tools --disable <name>` (repeatable) |
| `--enable` | `<TOOL>` | `tools --enable <name>` (repeatable) |
| `--path` | `<PATH>` | `skills --path <file-or-dir>`: the file (or skill directory) to read |
| `--text` | `<TEXT>` | `skills --text <body>`: the source's text, inline |
| `--name` | `<NAME>` | `skills --name <name>`: the source's name (default: the path's) |
| `--kind` | `<KIND>` | `skills --kind <skill\|instruction>` (default `skill`) |
| `--json` | nothing | `status --json`: print the answer as one JSON line instead of text |

### `kelir view`

Queue an image for the model's next message (`view <path>`)

| flag | takes | what it does |
|---|---|---|
| `<PATH>` | `<PATH>` | The image: `attachments/1-x.png` (resolved against the session dir first), or any path |

### `kelir bg`

Background jobs: run, ls, out, wait, cancel

| flag | takes | what it does |
|---|---|---|

### `kelir observe`

Print exactly what the model will see on its next request

| flag | takes | what it does |
|---|---|---|
| `<SESSION>` | `<SESSION>` | Session id, a unique prefix, or `latest` |
| `--raw` | nothing | Print only the exact JSON request body that would be sent |
| `--config` | `<CONFIG>` | Config file to use instead of the machine/project search |

### `kelir mcp`

MCP servers as a CLI: `ls`, `<server> ls`, `<server> <tool> [k=v …]`

| flag | takes | what it does |
|---|---|---|
| `<WORDS>` | `<WORDS>` | `ls`, `<server> ls`, or `<server> <tool>` and their flags |

### `kelir brief`

Ask a side question on a snapshot of this session (`brief "<question>"`)

| flag | takes | what it does |
|---|---|---|
| `<QUESTION>` | `<QUESTION>` | The question. Read from stdin when omitted |

### `kelir ls`

List sessions and children as a tree: state, depth, model, cache hits, task

| flag | takes | what it does |
|---|---|---|
| `--json` | nothing | Print the tree as JSON |
| `--all` | nothing | Every session on disk (the default: this session's tree, or the newest roots when run outside a session) |

`kelir ls sessions`, `kelir ls rename <old> <new>` and `kelir ls forget <name>`
are the named-session subcommands.

### `kelir ls sessions`

List named sessions: name, state, last activity, tokens

| flag | takes | what it does |
|---|---|---|
| `--json` | nothing | Print JSON instead of a table |
| `--named` | nothing | Only sessions that have a name |
| `--all` | nothing | Every session, children included |

### `kelir ls rename`

Give a session's name to another name; arguments `<OLD> <NEW>`.

### `kelir ls forget`

Drop a name; the session and its log stay. Argument `<NAME>`.

### `kelir kill`

Stop a child (and everything below it): `kill <session>` or `kill --tree`

| flag | takes | what it does |
|---|---|---|
| `<SESSION>` | `<SESSION>` | The child (session id or a unique prefix) to stop, with everything below it |
| `--tree` | nothing | Stop every descendant of this session (from a tool call), or of the session named |

### `kelir channel`

Headless channel bridge: serve conversations from channel MCP servers

| flag | takes | what it does |
|---|---|---|
| `--model` | `<MODEL>` | Model id for this run (overrides the config) |
| `--provider` | `<PROVIDER>` | Provider name: `openrouter` or `opencode-go` (there are exactly two) |
| `--cwd` | `<CWD>` | Working directory for the session (defaults to the current directory) |
| `--config` | `<CONFIG>` | Config file to use instead of the machine/project search |
| `--max-steps` | `<N>` | Cap one turn at N steps, then pause it (overrides `max_steps`). There is no cap by default; `0` also means no cap |
| `--server` | `<NAME>` | Serve only these channel servers (default: every `channel = true` server) |
| `--max-messages` | `<N>` | Stop after this many inbound messages have been answered (tests; default: forever) |
| `--status-once` | nothing | Dump one JSON status line at startup, then serve (for `ctl status` tests) |

### `kelir doctor`

Show which key each provider would use (never the key itself)

| flag | takes | what it does |
|---|---|---|
| `--model` | `<MODEL>` | Model id for this run (overrides the config) |
| `--provider` | `<PROVIDER>` | Provider name: `openrouter` or `opencode-go` (there are exactly two) |
| `--cwd` | `<CWD>` | Working directory for the session (defaults to the current directory) |
| `--config` | `<CONFIG>` | Config file to use instead of the machine/project search |
| `--max-steps` | `<N>` | Cap one turn at N steps, then pause it (overrides `max_steps`). There is no cap by default; `0` also means no cap |
| `--json` | nothing | One JSON object instead of the text report |

### `kelir docs`

Read kelir's own reference (config, CLI, verbs, files, …)

| flag | takes | what it does |
|---|---|---|
| `<TOPIC>` | `<TOPIC>` | The topic: `config`, `cli`, `verbs`, `env`, `events`, `files`, … |
| `--list` | nothing | List the topics and their one-line summaries |
| `--search` | `<WORDS>` | Case-insensitive search over all topics: `topic:line: text`, best topic first |
| `--skill` | `<NAME>` | Print a builtin skill (`kelir`, `delegation`, `herdr`) in full: they are compiled in and have no file to read |


## `kelir mcp …`

MCP servers as a CLI. clap only collects the words; `src/mcp/args.rs` parses
them, because after `kelir mcp <server> <tool>` the flags belong to the call
(`--json` means "the arguments, as a JSON object" there and "machine-readable"
after `kelir mcp ls`).

```text
kelir mcp ls [--json]
kelir mcp <server> ls [--detail name|desc|full]
kelir mcp <server> <tool> [k=v ...] [--json '{...}'] [--schema] [--envelope]
kelir mcp <server> call <tool> ...   (a tool named ls or call)
```

Exit codes: 0 ok, 1 the tool reported `isError`, 2 usage, 3 server
unavailable, 4 timeout (unknown outcome, never retried). The whole story —
`mcp.toml`, typing of `k=v`, the broker, channel servers — is `kelir docs mcp`.

## `kelir bg-shim`

Internal (hidden): the detachable job shim. Not for humans, so it is not in the
table above. `--dir` is the job root (`calls/` or the global `bg/`); the job owns
`<root>/<id>/`. `--session-dir` names the owning session for a session-less
`bg run` job, so its command can still reach `kelir bg`.
