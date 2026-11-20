# tui2 (STEER-10 + STEER-3) — implementer notes

Job: TUI v2 — progress clarity, collapsed tool calls, image paste pointers, full
markdown. One branch (`job/tui2`), one worktree, based on `main` at `ea9fc54`.
Design followed: `docs/design/TUI-v2.md`; deviations in `docs/DEVIATIONS.md`
("TUI v2" section).

## What landed (in commit order)

1. `theme.rs` — the one place styles and glyphs come from. Only the 16 ANSI
   colours and the terminal default; de-emphasis is `Modifier::DIM`, never
   `DarkGray`; `NO_COLOR` drops colours and keeps the attributes and glyphs;
   `KELIR_ASCII`/`TERM=dumb` swaps the Unicode set.
2. `activity.rs` — the state machine STEER-10 asks for: `idle`, `thinking`,
   `writing`, `running`, `rate limited`, `paused`, `error`, with start times,
   running-call previews, a retry countdown and the one-line turn summary. A
   pure state machine (transitions take an `Instant`), 11 unit tests; the
   state row and Herdr's OSC word both derive from it.
3. `markdown.rs` — the STEER-3 renderer on `pulldown-cmark`: headings,
   emphasis/strike, inline code, fences (dim gutter, language label, clipped
   not wrapped), nested/ordered/task lists, block quotes (bar on every wrapped
   row), tables (boxed when they fit, `a | b | c` below 30 columns), links,
   rules, HTML as text, images as their alt text. Width-aware wrapping over
   `Span` runs with the style carried across the break; a `Highlighter` seam
   with a no-op default. `Stream` splits the incoming text into complete blocks
   (fence and table aware) so a finished block is parsed once. 26 unit tests,
   including "every character boundary split renders the same as one pass".
4. `tools_view.rs` — one dim line per call with a right-aligned verdict, red for
   a failure plus one dim tail line, `ran N commands` for 3+ shell calls in a
   group (a failure or an `edit`/`write` keeps its own line), append-only
   `/expand` blocks and the collapsed/full policy. 9 unit tests.
5. `attachments.rs` — pasted-path parsing (quotes, `& '…'`, `file://`, `%20`,
   `\ `), image dimensions from the header by hand (PNG IHDR, JPEG SOF, GIF,
   WebP VP8/VP8L/VP8X), saving to `<session>/attachments/<seq>-<name>` with
   content dedupe and a 10 MB cap, the pointer text, and base64 for the wire.
   11 unit tests.
6. `editor.rs` — `Chip`: an image in the input as one atomic element
   (`[image 1]`), removed whole by backspace, expanded to the pointer on
   submit, its file deleted if it is taken back before sending. 4 unit tests.
7. `tui.rs` — the rows (3 live / state / 3 editor / facts), the live markdown,
   the dim prompt while working, the spinner/elapsed/retry tick that only exists
   while working, synchronised updates around every frame and every
   `insert_before`, clear-on-resize, `Esc` to stop, the group management, chips,
   `/expand` and `/collapse`.
8. `kelir view <path>` — a CLI reached through `bash` (no tool-schema cost) that
   sends the `view` verb to the live session over `KELIR_SOCK`; the session
   validates it in the ack, copies the image into the session, records a
   `source` event (`kind: "attachment"`, relative path) at the tail. The fold
   turns that event into a user message with one `image_url` part, so every
   earlier message stays byte-identical (`ChatMessage::to_wire`).
9. Docs: `docs/reference/tui.md`, `cli.md`, `env.md`, plus the deviation notes.

Tests: 363 lib unit tests (was 325 at the base), 22 `e2e_tui` (was 15), a new
`e2e_tui2` (2, image flow against the mock), everything else unchanged and
green. Windows: fmt, clippy `-D warnings`, full `cargo test` three times, no
failures. Linux (the maintainer, `kelir-verify-tui2`): fmt, clippy, all 20 suites green.
Binary: 6.38 MB → 6.89 MB (+0.49 MB; the budget was 0.7 MB).

## What was hard

- **The "streamed render equals the one-shot render" invariant** cost the most
  time. Three bugs, each found by a test: a break that ate the character which
  triggered it; a table's rows being cut into separate blocks (so a table
  rendered as three one-row tables); and blank rows between blocks being added
  *inside* the block renderer, so joining blocks doubled them. The fix that made
  the invariant true by construction: `render` is now `Stream` + one blank row
  between blocks + `render_block`, and the streaming path is literally the same
  three steps.
- **`heredoc`-written files kept truncating** in this shell session (the tool
  input is cut at ~8-11 KB with no error line), which is why several files were
  written in chunks with `edit`. Costly and easy to miss: one truncated file
  still compiled (a `match` arm cut in half), and the first `theme.rs` write
  ended mid-`fn`. Worth knowing for the next long file.
- **rustfmt versions differ between the dev box (1.10.0) and the maintainer (1.11.0)**:
  `cargo fmt --check` passed here and failed there on three hunks. Formatting by
  hand to what 1.11 does was faster than adding a `rustfmt.toml` (which would
  change every file).
- **A `stash`/`checkout <base> -- .` sequence I ran to investigate a flake
  wiped uncommitted test work.** Recovered from the dangling stash commit with
  `git show <sha>:tests/e2e_tui.rs`; nothing was lost in the end, but the lesson
  is: never `checkout <rev> -- .` in a worktree with uncommitted work, and never
  `stash` to "look at the baseline" (a second clone is the safe way).
- **`kelir view` first validated at the boundary, not in the ack**: the CLI
  printed "image queued" for a missing file because the ack only logged the
  verb. Moving the check into `accept` (`check_view`) made the CLI's answer the
  truth, and the e2e — which asserts the refused verb reads `boundary:
  "rejected"` — is what caught it.
- **The pty tests needed the raw stream stripped of escapes** before asserting on
  text; the first version of my stripper stopped on the `[` of a CSI (it is in
  the 0x40..=0x7e final-byte range), which looked like the TUI emitting garbage.

## What the prompt / tools got in the way of

- **STEER-5 (retry) is a parallel job.** The `Retry` event, the countdown and
  the state are all here and tested; the session does not emit it yet because
  that is STEER-5's loop. It is one `emit` when it lands. (Noted in
  DEVIATIONS.)
- **STEER-11 (`docs/reference/` + drift test) is also parallel.** I wrote
  `docs/reference/{tui,cli,env}.md` in the STEER-11 format it specifies, but the
  drift test and the `README.md` index are that job's; a merge will need one of
  them to own the index.
- **The design note asks to "confirm" three things** (inline viewport height
  mutability, the clipboard features' cost, the narrow-pane behaviour). The
  first has no cheap answer in ratatui 0.30 (recreating the terminal is the only
  way, and it is the flicker the note forbids), so the rows are re-allocated
  instead and it is recorded. The second is affordable but untestable in CI;
  path paste ships and the clipboard is a documented stub. The third is
  snapshot-tested at 80/60/40/30/20.
- **The `tui.rs` file is large** (the old one was 1492 lines; the new one is
  similar with more behaviour). Splitting it further (a `tui/live.rs`,
  `tui/draw.rs`) would have made the diff harder for the reviewer to follow for
  no functional gain; the pieces that are testable alone were moved out
  (`theme`, `activity`, `markdown`, `tools_view`, `attachments`), which is what
  actually mattered.

## Known gaps (for the reviewer)

- Clipboard image on Ctrl-V: not implemented (stub + message). Path paste, drag
  path and `kelir view` all work.
- `Retry` UI is wired but no session event sends it until STEER-5 merges.
- The frame cap is structural (drain-then-paint), not a 33 ms timer.
- A committed tool line cannot be unfolded; `/expand` appends. Documented in the
  help text and in DEVIATIONS.
- Live drive (a real model, screenshots, `NO_COLOR=1`, a light theme, pane
  narrowing) is the reviewer's step, not done here.
