# CLI (subcommands and flags)

**Summary:** `kelir` with no subcommand is the interactive session; `run`,
`ask`, `resume`, `ctl`, `view` and `bg` are the others. The agent reaches the
CLIs below through `bash`, so none of them costs tool-schema text.
**Read this when:** scripting kelir, or adding a subcommand (`src/cli.rs`,
`src/main.rs`).

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

Background jobs (`docs/reference/verbs.md` has the events).

## `kelir mcp …`

MCP servers as a CLI.

## `kelir bg-shim`

Internal (hidden): the detachable job shim.
