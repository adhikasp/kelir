# The TUI (keys, commands, states)

**Summary:** the interactive session (`kelir`, no subcommand) is an inline
8-row view: the live area, a state row, the editor and a facts row, with
finished turns written above it into the terminal's own scrollback.
**Read this when:** you are driving kelir by hand, or changing `src/tui.rs`,
`src/theme.rs`, `src/activity.rs`, `src/markdown.rs`, `src/tools_view.rs` or
`src/attachments.rs`.

Code: `src/tui.rs` (the app, rows, keys, loop), `src/activity.rs` (the state
machine), `src/theme.rs` (colours and glyphs), `src/markdown.rs` (the
renderer), `src/tools_view.rs` (tool lines and groups), `src/attachments.rs`
(images), `src/commands.rs` (`/` commands), `src/editor.rs` (the buffer and its
image chips).

## Rows

| row | what it holds |
|---|---|
| live (3) | the tail of the streaming answer (markdown) or the open tool group |
| state (1) | `● idle · ready`, `⠹ thinking 12s · waiting for the provider · esc stops`, … |
| editor (3) | `› your message`; the prompt dims while the agent works |
| facts (1) | `kelir working · model · step · 1.5k tok · 75% cache · $0.0004` |

A shorter terminal gives rows up in this order: the facts row, live rows, the
editor down to one row; the state row is the last to go.

## States (the state row)

| state | glyph | looks like |
|---|---|---|
| idle | `●` | `● idle · ready` — nothing animates, no wakeups |
| thinking | spinner | `⠹ thinking 12s · waiting for the provider · esc stops` |
| writing | spinner | `⠹ writing 7s · ~214 tok · esc stops` |
| running a tool | spinner | `⠹ running bash 12s · sleep 45 && echo done` |
| several tools | spinner | `⠹ running 3 tools 12s · bash: cargo test` |
| rate limited | spinner | `⠹ rate limited · 429 · retrying in 34 s · attempt 5 · say stop` |
| paused | `⏸` | `⏸ paused · step cap 50 reached · say continue` |
| error | `✗` | `✗ error · provider 401 · say to retry` |

A turn that ends **paused** or **in error** keeps its row after it ends (with
the step cap or the provider's reason and the next action); `done` and
`stopped` go back to `● idle · ready`, and the one-line summary above the row
says which of the two it was. The facts row says `kelir idle` for a finished
turn, `kelir paused`/`kelir error` for the two that need a human: one word for
one state.

Spinner and elapsed time exist only while working. Herdr's OSC 7501 word comes
from the same state: thinking/writing/running/rate limited → `working`,
paused → `blocked`, error → `error`, done → `done`, idle → `idle`.

## Keys

| key | does |
|---|---|
| Enter | send; while working it is a `say` (the prompt dims to say so) |
| Alt-Enter, Shift-Enter, Ctrl-J | newline |
| Esc | stop the turn (only while working) |
| Ctrl-C once | stop the turn; twice within 1 s quits |
| Ctrl-O | the last tool call's output tail (`Esc`/`Ctrl-O` closes) |
| Ctrl-L | redraw |
| Ctrl-K / Ctrl-U | cut to end of line / clear |
| ↑ / ↓ | history (first line ↑, last line ↓) |
| Backspace on an image chip | removes the whole chip and deletes its file (a plain Backspace on text still deletes one character) |

## `/` commands

| command | does |
|---|---|
| `/stop [reason]` | stop the turn now |
| `/say-now <text>` | stop the running calls, then send the message |
| `/later <text>` | queue a message for the end of the turn |
| `/model <p/m>` | swap provider/model for the rest of the session |
| `/tools off\|on <name>` | disable or enable `bash`/`edit` |
| `/compact [focus]` | summarise the earlier context |
| `/expand` | from the next call on, print each call's output too |
| `/expand [n\|last]` | append call `n`'s output here (a committed line cannot be unfolded) |
| `/collapse` | one line per tool call from the next call on |
| `/help` | the list |
| `/quit` | end the session |

Each of the three is idempotent and has one direction: `/collapse` never
expands, and saying either one twice changes nothing.

## Spacing

A prompt echo, a tool group, prose and the end-of-turn summary are separate
blocks with **one** blank row between them and never two.

## Tool lines

A settled call is one dim line with a right-aligned verdict:
`  $ cargo test            ✗ exit 101 · 4.1 KB · 4.1s`. A failure is red, keeps
its line and gains one dim tail line with the last output line. Three or more
shell calls in one group fold into `  ran 3 commands · 3.2s`; a failed call, an
`edit` and a `write` always keep their own line. Prose is the only
default-weight text.

## Markdown

Assistant text is rendered: headings (no `#`), bold/italic/strike, inline code,
fenced code (dim gutter, language label, clipped not wrapped), nested and
ordered lists, task boxes, block quotes (bar on every wrapped row), tables
(boxed when they fit, `a | b | c` below 30 columns), links (text underlined,
` (url)` when different), rules, and raw HTML as literal text. Colours are the
16 ANSI ones; `NO_COLOR` keeps the attributes and the glyphs; `KELIR_ASCII=1`
or `TERM=dumb` uses ASCII for the glyphs the design note names (the spinner,
`●`, `⏸`, `✗`, `✓`, the bullets, the code gutter, the quote bar and the table
borders). The banner, the prompt `›`, the `»` of a verb reply and the ` · `
separators stay Unicode in both sets: they are decoration, not state, and
`NO_COLOR`/`KELIR_ASCII` exist to keep the *state* readable rather than to make
every byte ASCII.

Markdown is rendered **block by block as it streams**: a closed block (a
paragraph, a fence, a table) goes straight into the scrollback, and only the
block still growing is live. So a 60-line code block scrolls into history as it
arrives instead of becoming a three-row ticker, nothing that was shown ever
vanishes, and a loose list item's continuation paragraph keeps its indent
(the block splitter joins it back to its item).

## Images

Paste or drag a path to a `.png/.jpg/.jpeg/.gif/.webp`: it becomes a chip
`[image 1]` in the editor and the prompt carries
`[image: attachments/1-shot.png, 1280x720, 312 KB — run: kelir view attachments/1-shot.png]`.
The bytes are copied to `<session>/attachments/<n>-<name>.<ext>` (numbered in
the order they arrive); the model looks with the `kelir view` the pointer names
(see `docs/reference/cli.md`), which queues a `source` event (kind
`attachment`) at the tail of the conversation. Nothing earlier is edited, so the
provider's cached prefix stays byte-identical. The `user` event's
`attachments[]` lists the paths the pointers name.

Three ingresses, one result:

1. **A bracketed paste** of a path (`Event::Paste`, Unix): the chip appears as
   the paste arrives.
2. **A path typed as keystrokes** (a Windows terminal, where bracketed paste
   arrives as key events; also a drag and drop): the moment the text that ends
   at the cursor is a real image file, the same chip replaces it — so the chip,
   its reversed label and its one-Backspace removal behave the same on every
   platform. A partial path stays text until it is a file.
3. **`kelir view <path>`** from any shell for an image the prompt did not name.

Backspace on a chip removes the chip and its saved file; a chip that was sent is
the model's pointer and its file stays. `kelir view` refuses a file whose bytes
are not an image (signature and header are read, not the extension) and refuses
a model the provider marks as text-only, in both cases without touching the
conversation.
