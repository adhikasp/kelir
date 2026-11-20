# tui2 plan (implementer scratch, committed as the job's own map)

Build order (design note section 8):
1. theme.rs (NO_COLOR, ASCII glyphs, 16 ANSI colours, DIM)
2. activity.rs (state machine: idle/thinking/writing/running/rate limited/paused/error)
3. tui.rs: 3 live rows + state row + 3 editor rows + facts row, dim prompt, turn summary line,
   spinner/elapsed only while working, sync updates, 33 ms frame cap, resize clear.
4. tools_view.rs: one dim line per call, groups (3+ calls -> `ran N commands`), errors red + tail,
   /expand [n], /collapse, Ctrl-O overlay.
5. markdown.rs: pulldown-cmark block model, width-aware wrap, tables, quotes, fences, tasks,
   stream splitter (block at a time), Highlighter seam.
6. attachments.rs + cli `kelir view` + `source` kind attachment at the tail: path paste -> chip ->
   pointer text; no image bytes in earlier messages.
