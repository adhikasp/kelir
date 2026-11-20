# CLI (subcommands and flags)

**Summary:** `kelir` with no subcommand is the interactive session; `run`,
`ask`, `resume`, `ctl`, `view`, `bg`, `doctor` and `docs` are the others. The
agent reaches the CLIs below through `bash`, so none of them costs tool-schema
text.
**Read this when:** scripting kelir, or adding a subcommand (`src/cli.rs`,
`src/main.rs`).

Every command takes clap's own `--help`, which is the same table one screen
away; the block below is generated from clap so a flag cannot exist in one and
not the other. Exit codes: `0` success, `1` a refusal or a failed run, `2` a
usage error (a bad flag, an unreadable config, an unknown provider).

## `kelir` (no subcommand), `kelir tui`

The interactive session. Flags: `--model`, `--provider`, `--cwd`, `--config`,
`--max-steps`, `--resume <ID>`. Keys and rows: `docs/reference/tui.md`. Without
a TTY it is a usage error (exit 2).

## `kelir run [TASK] [--json] [--keep] [--resume ID]`

Run one task to completion. `--json` streams the event log as NDJSON on stdout;
`--keep` stays alive between turns (accepting `say`/`later`/`end`).

## `kelir ask <QUESTION>`

One question, the final text on stdout.

## `kelir resume <ID> [TASK]`

Continue a session from its event log; never re-runs a tool call.

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
session on purpose: its shim writes `bg/<id>.status`, and the next `resume`
folds a job that settled meanwhile into the log.

## `kelir doctor [--json] [--provider <name>]`

Which key each provider would use, and where it comes from: the source, never
the secret. Exit 0 when every provider that matters has a usable key, 1 when the
session's own provider (or an expired credential) does not.

## `kelir docs [TOPIC] [--list] [--find <words>] [--generate] [--check]`

The reference, embedded in this binary with `include_str!`, so it works in a
session with no checkout on disk. `--list` names the topics; a bare `kelir docs`
is the index; `kelir docs <topic>` prints one file; `--find <words>` is a
ranked search over them.

`--generate` rewrites the blocks between `<!-- generated:kind -->` markers from
the code, and `--check` fails when one is stale: those two are for a person
with a checkout (`docs/reference/` under the current directory, or the
directory `KELIR_DOCS_ROOT` names) and are the only part of `docs` that touches
a file. Exit codes: `0` a read or a clean `--check`, `1` an unknown topic or a
search with no hit (the message suggests a topic), `2` `--generate` together
with `--check`.

The whole surface, generated:

<!-- generated:cli -->
### `kelir run`

Run a task to completion

| flag | takes | what it does |
|---|---|---|
| `--model` | `<MODEL>` | Model id for this run (overrides the config) |
| `--provider` | `<PROVIDER>` | Provider name: `openrouter` or `opencode-go` (there are exactly two) |
| `--cwd` | `<CWD>` | Working directory for the session (defaults to the current directory) |
| `--config` | `<CONFIG>` | Config file to use instead of the machine/project search |
| `--max-steps` | `<N>` | Cap one turn at N steps, then pause it (overrides `max_steps`). There is no cap by default; `0` also means no cap |
| `<TASK>` | `<TASK>` | The task. Read from stdin when omitted |
| `--json` | nothing | Stream the event log as NDJSON on stdout |
| `--keep` | nothing | Stay alive after the turn ends, accepting `say`, `later` and `end` |
| `--resume` | `<ID>` | Continue a session from its log instead of starting one |

### `kelir ask`

Ask one question and print the final text

| flag | takes | what it does |
|---|---|---|
| `--model` | `<MODEL>` | Model id for this run (overrides the config) |
| `--provider` | `<PROVIDER>` | Provider name: `openrouter` or `opencode-go` (there are exactly two) |
| `--cwd` | `<CWD>` | Working directory for the session (defaults to the current directory) |
| `--config` | `<CONFIG>` | Config file to use instead of the machine/project search |
| `--max-steps` | `<N>` | Cap one turn at N steps, then pause it (overrides `max_steps`). There is no cap by default; `0` also means no cap |
| `<QUESTION>` | `<QUESTION>` | The question. Read from stdin when omitted |

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

### `kelir resume`

Continue a session from its log (never re-runs a tool call)

| flag | takes | what it does |
|---|---|---|
| `--model` | `<MODEL>` | Model id for this run (overrides the config) |
| `--provider` | `<PROVIDER>` | Provider name: `openrouter` or `opencode-go` (there are exactly two) |
| `--cwd` | `<CWD>` | Working directory for the session (defaults to the current directory) |
| `--config` | `<CONFIG>` | Config file to use instead of the machine/project search |
| `--max-steps` | `<N>` | Cap one turn at N steps, then pause it (overrides `max_steps`). There is no cap by default; `0` also means no cap |
| `<ID>` | `<ID>` | Session id, or a unique prefix of one |
| `<TASK>` | `<TASK>` | A message to add before the turn continues. Read from stdin with `-` |
| `--json` | nothing | Stream the event log as NDJSON on stdout |
| `--keep` | nothing | Stay alive after the turn ends |

### `kelir ctl`

Send a verb to a running session and print its answer

| flag | takes | what it does |
|---|---|---|
| `<SESSION>` | `<SESSION>` | Session id, a unique prefix, or `latest` (the newest session only; an error if it is not running) |
| `<VERB>` | `<VERB>` | The verb: say, later, stop, model, tools, compact, skills, view, cancel, status, end |
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

### `kelir mcp`

MCP servers as a CLI: `ls`, `<server> ls`, `<server> <tool> [k=v …]`

| flag | takes | what it does |
|---|---|---|
| `<WORDS>` | `<WORDS>` | `ls`, `<server> ls`, or `<server> <tool>` and their flags |

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
| `--find` | `<WORDS>` | Ranked search over every topic: which file mentions these words |
| `--generate` | nothing | Rewrite the generated blocks (`cli`, `env`, `events`, `verbs`, `config`) from the code. Needs a checkout |
| `--check` | nothing | Fail when a generated block is stale: CI and the drift test |

<!-- /generated -->

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
generated block above.
