# tui2fix — implementer's notes (tui2 review fixes)

Scope: `docs/tasks/STEER-10-fixes.md` (2 blockers, 8 should-fix, 7 nits from
`docs/reviews/tui2-review.md`). Branch `job/tui2`, worktree `kelir-wt/tui2`.

## What the fixes came to

The two blockers had one root cause each, and both were about *when* a row is
written rather than how it is rendered.

**B1 / S1 — the message is now committed a block at a time.** The old model
kept every streamed block in `pending_rows` until `Assistant` arrived, so
(a) a tool group that settled in the middle of a turn was appended to the
scrollback *after* prose that had already gone out (`B1`, the transcript lied
about order), and (b) a long block sat in a 3-row live window, ticker-style,
while the block before it was in neither place (`S1`). `App` now holds a
`Message` of `Block`s: each closed block's rows are committed immediately, the
open tail is rendered by the same function that will commit it, and `committed`
counts what has gone out so no row can be written twice. Three further
consequences fell out of that:

- A tool call **interrupts prose**: `ToolStarted` (and a settle with no start,
  on a resume) flushes the stream into a block first, so the piece of paragraph
  that announced the call stays above the call even when the model put no blank
  line before or after it. The live drive found this one: the first fix covered
  `Assistant` but not a paragraph that spans a call.
- Notices are prose too: `Paused`, `Error`, `Info` and `Attachment` close the
  open group before they are queued. The paused drive showed
  `turn paused after 2 steps` landing between two tool lines.
- `Message::settle` drops a held block's "may be continued" flag on a call,
  because a call is not a paragraph: nothing in a message continues across one.

**S7 — the loose list item.** The block splitter cuts at blank lines;
CommonMark does not (an indented continuation stays in the item). `render` and
the streamed path both join blocks through `markdown::join_blocks`
(`ends_in_item` + `continues_item`), and the renderer keeps the item's hanging
indent for its continuation lines (`pending_prefix`), including for a fence
inside an item. The streamed-equals-one-shot invariant still holds for every
split point of the reviewer's corpus.

**B2 / S8 — the chip is a chip on Windows.** Backspace is `Key::RemoveChip`
first with a `Key::Backspace` fallback, so text never loses a keystroke. More
importantly the chip no longer depends on `Event::Paste`: every typed character
is followed by one cheap check of the text ending at the cursor
(`attachments::path_tail`, no file system until the extension matches), and the
moment it is a real image file the path is replaced with the same chip
(`editor::take_word_before_cursor` takes the word or the whole quoted run).
That is what makes the maintainer's platform equal: the drive showed `› look
[image 1]`, one Backspace, `› look`, and the file gone.

**S2/S3 — `kelir view` tells the truth before it spends.** The model list is
read through three shapes (a Go capability flag, OpenRouter's
`architecture.input_modalities`, a named parameter) and silence is "try it",
because reading an absent `supported_parameters` entry as "no images" would
have refused every vision model on OpenRouter. The bytes are checked
(signature + readable header, first 64 KB): a `.png` that is text, or an empty
file, is refused before a `source` event exists, because such an event makes
every later request fail.

**S4 — discoverability.** One line in `src/prompt.md` (909 B, budget 1 KB,
snapshot test) and the pointer itself says `— run: kelir view <path>`.

**S5/S6 — the small deceptions.** A paused or failed turn keeps its state row
(with the reason and the next action) until the next turn; `done`/`stopped`
still go idle, and the facts row now shares the state row's word (`kelir idle`,
not `kelir done`). `/expand` and `/collapse` each set the policy one way,
idempotently; `/expand n` only appends.

**Nits.** N1 one blank row between blocks (never two — the pty repaint had been
hiding several). N2 prefixes are clipped to the pane, so no row is wider than
the layout at 2..120 columns. N3 the ASCII set's scope is documented. N4 `1
step`. N5 `attachments[]` on the `user` event. N6 numbered copies. N7: this
job's rows name no machine.

## Verification

- Unit: 378 lib tests (was 367). New: the three turn shapes' row order, the
  notice ordering, the keystroke burst (chip / partial path / quoted path /
  re-chip after removal), `sniff`, `path_tail`, `paths_in`, `next_seq`, the
  vision parse, prefix clipping.
- e2e: `e2e_tui` 24 (new: a typed path is a chip *before* Enter in a pty),
  `e2e_tui2` 2, every other suite green.
- The reviewer's `tests/review_tui2.rs` is **fully un-ignored** (13 tests, 0
  ignored) and green.
- Linux (`scripts/remote-check.sh <machine> kelir-verify-tui2fix`): fmt, clippy
  `-D warnings`, 383 lib + every suite, 0 failed.
- Live drive (release binary in a pty on Windows, pyte replay of the raw
  stream): a tools-then-answer turn in both orders, a 60-line fence (60 rows,
  each once, in history mid-stream), a step-cap pause and a 401 (the rows stay),
  a chip typed and removed by Backspace then sent (pointer with the hint in the
  request body), a 40-column `NO_COLOR` pane. On Linux: a real bracketed paste
  chip, Backspace, and the pointer in the request.

## Known gaps (kept honest)

- A loose list item's last block waits one block (that is the S7 price).
- The state row's token count is still an estimate (`~N tok`).
- `Retry`/429 still waits for STEER-5's event (unchanged deviation).
- The Windows clipboard image (Ctrl-V) is still a documented stub.
- Deeply nested list indents collapse at very narrow widths (clipped, not
  wrapped): readable, and no row overflows.
