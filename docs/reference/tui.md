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
| editor (3) | `› your message` in the user colour (cyan); the prompt dims while the agent works |
| facts (1) | `kelir working · model · 1.5k tok` — steps, cache and cost are not ambient |

A shorter terminal gives rows up in this order: the facts row, live rows, the
editor down to one row; the state row is the last to go.

## The banner

The banner is `kelir · provider · model`, `session <id>`, `cwd <path>`, the
help line, the step cap when set, and a `resumed …` line after a resume. It carries **no** window line: the window compaction
measures against is not ambient, and `state` (every `kelir ctl` verb's answer)
reports `window`, `window_source` and `window_note` for anyone who asks.

The one exception is a window that is **not** the provider's, because that
silently changes when the session compacts: a default fallback (an offline
endpoint, a 401, a list that never landed) adds one dim line under `cwd`,
`window 128000 tokens (default) · the model list: offline`. It is said once:
when the start already knows, the banner carries it; otherwise the line appears
when the answer that failed lands (`» window 128000 tokens (default) · …`). A
`/model` swap that changes the window says so the same way, once, because the
banner has no window line to update: `» window 200000 tokens (api)`.

While the session waits before trying the provider again (a retryable HTTP
status) or for a concurrency-governor slot, the facts row shows the wait
instead of the usual summary: `kelir working · rate limited (429): retrying in
34 s, attempt 5, esc stops · 33 s left`. The countdown is read from the
wall clock on each tick, so it moves; a slot wait has no deadline worth reading
and shows its words only. A provider retry also turns the state row `rate
limited` (see below); a slot wait leaves the state row on the work it was doing.
The same wait is one dim line in the scrollback (`» rate limited (429): …`,
with the same `esc stops` hint) and an `error` event in the log, so NDJSON, the
pane and the transcript agree. The log and the CLI keep STEER-5's sentence
(`say stop to cancel`), which is true for `kelir` driven from a shell: only the
pane's rows name the key, because plain text typed into the prompt is a `say`
the model receives, while Esc and Ctrl-C stop the turn and `/stop` ends it at
the next boundary or during the wait.

## States (the state row)

| state | glyph | looks like |
|---|---|---|
| idle | `●` | `● idle · ready` — nothing animates, no wakeups |
| thinking | spinner | `⠹ thinking 12s · waiting for the provider · esc stops` |
| writing | spinner | `⠹ writing 7s · ~214 tok · esc stops` |
| running a tool | spinner | `⠹ running bash 12s · sleep 45 && echo done` |
| several tools | spinner | `⠹ running 3 tools 12s · bash: cargo test` |
| rate limited | spinner | `⠹ rate limited · 429 · retrying in 34 s · attempt 5 · esc stops` |
| paused | `⏸` | `⏸ paused · step cap 50 reached · say continue` |
| error | `✗` | `✗ error · provider 401 · say to retry` |

A turn that ends **paused** or **in error** keeps its row after it ends (with
the step cap or the provider's reason and the next action) and writes one
outcome line (`⏸ paused · 3 steps · 4.8k tok`, no cost or timing); `done` ends
on its prose and goes back to `● idle · ready`, and `stopped` writes one line
and does the same. The facts row says `kelir idle` for a finished turn,
`kelir paused`/`kelir error` for the two that need a human: one word for one
state.

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
| `/say-now <text>` (`/say_now`, `/now`) | stop the running calls, then send the message |
| `/later <text>` | queue a message for the end of the turn |
| `/model <p/m>` | swap provider/model for the rest of the session |
| `/tools off\|on <name>` | disable or enable `bash`/`edit` |
| `/compact [focus]` | summarise the earlier context |
| `/expand` | from the next call on, print each call's output too |
| `/expand [n\|last]` | append call `n`'s output here (a committed line cannot be unfolded) |
| `/collapse` | one line per tool call from the next call on |
| `/help` (`/?`) | the list |
| `/quit` (`/exit`) | end the session |

Each of the three is idempotent and has one direction: `/collapse` never
expands, and saying either one twice changes nothing.

## Spacing

A prompt echo, a tool group, prose and the outcome line (paused, stopped,
error) are separate blocks with **one** blank row between them and never two.
The human's own words in the scrollback are cyan, so a long session can be
scanned for what was asked. Cyan is the terminal's ANSI 6: a terminal whose
palette remaps it light loses contrast on a light background. Change the
palette, not the transcript.

## Tool lines

A settled call is one dim line with a right-aligned verdict:
`  $ cargo test            ✗ exit 101 · 4.1 KB` (no duration: the transcript
is not a profiler). A failure is red, keeps its line and gains one dim tail
line with the last output line. One or two calls in a group (a run of calls
between two pieces of prose) keep one line each. A `bash` call's optional
`description` (one line, <= 120 chars) is its head in every view; otherwise
`$ <command>`. The command stays in the log.

Three or more calls collapse into one checked line, and that line names what
it does **not** stand in for:

```
  ✓ 2 calls · 1 failed · 1 stopped
    $ cargo test                    ✗ exit 101 · 4.1 KB
    error[E0432]: unresolved import `foo`
    $ sleep 20                      ⏹ cancelled
    edit src/lib.rs                 ok · 1.2 KB
```

The checked count covers only cleanly finished calls the fold hides. A failure,
a stopped call (Esc) and a file change keep their own line — a stopped call is
never counted, and the count adds up (`2 calls · 1 failed · 1 stopped`), so a
reader can trust it. `edit` (and any non-`bash` tool) never folds away: which
file an agent touched is what a reader wants. With nothing cleanly finished
there is no `✓` line. Prose is the only default-weight text.

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

Markdown is rendered **block by block as it streams**, and within a block **row
by row** wherever a row is already final:

- A fence commits every line that has ended, keeping the last three lines in
  the live area, so a 60-line code block scrolls into history as it arrives
  instead of becoming a three-row ticker.
- A top-level list commits every item that has ended (the cut is just before a
  new top-level item), keeping three lines live, so a long list fills the
  scrollback too. A fence *inside* an item is held with the item, because its
  indent belongs to that item's render.
- A paragraph and a table commit when the block closes: a paragraph's wrap
  depends on all of it, and a boxed table's column widths on every row it has.

A closed block is rendered once, when it closes, and its rows are cached, so
the work per delta does not grow with the message: the open block is the only
thing re-rendered, and it holds at most the last three live lines. Nothing that
was shown ever vanishes, and a loose list item's continuation paragraph keeps
its indent (the block splitter joins it back to its item) with one blank row
between the item's paragraphs.

## Images

Paste or drag a path to an image file: it becomes a chip
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

The format is read from the bytes, not the name (`png`, `jpg`, `gif`, `webp`),
so a text file named `fake.png` makes no chip — nothing promises an image the
tool would refuse. Also, when the image is sent, its `data:` media type is the
sniffed format, never the extension.

Backspace on a chip removes the chip and its saved file; a chip that was sent is
the model's pointer and its file stays. `kelir view` refuses a file whose bytes
are not an image (signature and header are read, not the extension) and refuses
a model the provider marks as text-only, in both cases without touching the
conversation.
