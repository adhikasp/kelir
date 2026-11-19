# STEER-3 — render the agent's markdown in the TUI

## Goal
Assistant text in the inline TUI (live area and scrollback) is rendered as markdown: headings,
emphasis, inline code, fenced code blocks, lists (nested, ordered, task), block quotes, tables,
links, rules. Tool output, user text and the status line stay plain. `kelir run` (non-JSON) may use
the same renderer when stdout is a TTY; `--json` is untouched.

## Recommended approach (the pick; deviate only with a reason in DEVIATIONS.md)
- Parse with `pulldown-cmark` (small, pure Rust, streaming-friendly); render into a small
  intermediate "styled lines" model, then into ratatui `Line`/`Span`. Do not pull a full markdown
  TUI crate. **No syntax-highlighting engine in v1** (syntect costs binary size and startup);
  fenced code is indented, dimmed-border or reverse-labelled with the language, monochrome.
  Leave a clean seam (`trait Highlighter`) for a later optional feature.
- Colours: the 16 ANSI colours only, so the user's terminal theme decides; respect `NO_COLOR`;
  a config flag `markdown = true|false` (default true) and a plain fallback.
- Streaming: the answer arrives in chunks. Render completed blocks as markdown; render the
  in-flight tail tolerantly (an unclosed fence is shown as a code block, an unclosed `**` stays
  literal until closed). Rendering must be incremental per block (no re-parse of the whole message
  per frame: the earlier wrap work removed an O(n²), do not reintroduce one).
  Invariant: the final render of a streamed message equals the one-shot render of the same text.
- Wrapping: width-aware (`unicode-width`, CJK/emoji), styles preserved across wraps, list indents
  and block-quote bars preserved on wrapped lines, code blocks not re-wrapped (hard-clip with a
  visible continuation marker or horizontal fit, pick one and document), tables shrunk to the width
  (truncate cells with an ellipsis) and degrade to plain rows when narrower than ~30 columns.
- Scrollback: committed lines go through `insert_before` styled; scrollback cannot reflow after a
  resize (accept and document), the live area re-renders on resize.
- Footprint: report binary size delta (budget <= 0.7 MB) and idle RSS (budget unchanged <= 30 MB).

## Tests (pyramid)
- Unit: parser → model snapshots for each element and nesting (list in quote in list, code in
  list, table with CJK, link/image, autolink, escaped chars, HTML blocks shown as text, raw
  `<`), wrapping with style continuity, NO_COLOR, width edge cases (1, 10, 80, 300 columns).
- Property/stream: split a corpus of markdown documents at every byte boundary (and every UTF-8
  boundary) into chunks; the final render equals the one-shot render; no panic; mid-fence and
  mid-table chunks render sanely.
- TestBackend snapshots of the inline area (streaming list, streaming fence, table, narrow).
- PTY e2e against the mock server: scripted answer with every element; assert the ANSI stream
  contains the expected style codes and that finished messages land in scrollback.
- Windows and Linux (`scripts/remote-check.sh`), no leaked processes (AGENTS.md rule).

## Live drive (reviewer)
In a Herdr pane, a real model (cheap default) asked for a markdown-heavy answer (headings, nested
lists, a table, a code block, a quote, inline code, a link, a task list); capture the pane
(screenshot via CopyFromScreen of the monitor, cropped) and judge legibility; resize the pane
while streaming; ctrl-c mid-fence; very long lines; wide characters; `NO_COLOR=1`.
