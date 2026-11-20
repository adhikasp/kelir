# tui2fix2 — round-2 review fixes (`docs/tasks/STEER-10-fixes2.md`)

Brief: the four should-fix findings of `docs/reviews/tui2b-review.md` (`S1b`,
`P1`, `R5`, `S3b`) plus the cheap nits, in worktree `kelir-wt/tui2`, branch
`job/tui2`. Started with `git merge main` (main had moved: `buildinfra`), then
the work below.

## S1b — the open block is no longer a three-row ticker

The round-1 fix committed *closed* blocks; a fence, a list or a table that was
still open stayed in the live area, so a 60-line fence showed only its last
three rows until the closing fence, and the rows above were nowhere.

Now a streamed block is committed **row by row wherever a row is already
final** (`markdown::Shape` classifies the open block):

| shape | what is final before the block ends |
|---|---|
| `Fence` | every complete line (code is clipped, never wrapped), so each line goes out as it ends, keeping `LIVE_KEEP = 3` lines live |
| `List` | every top-level item that has ended (the cut is just before a new top-level item), keeping three lines live; pieces carry the numbering (`first_number`, `ordinal_start`) so `3.` still reads `3.` |
| `Whole` | nothing: a paragraph's wrap depends on all of it, and a boxed table's column widths on every row |

`Stream::settle(layout, keep)` hands out the final rows; `Stream::settle` is
called after every delta (`App::commit_streaming`). The live area renders only
the bytes **not yet delivered** (`markdown::open_rows` from
`Stream::delivered_bytes`), so the block is never rendered twice.

The pty drive of a 60-line `rust` fence at 80 and 40 columns: `mid1` shows 32
fence rows in the scrollback plus the tail in the live area, `mid2` 69, and at
the end all 60 lines are present **exactly once, in order** (checked by
counting `fn f01…f60` in the replay — no duplicate, no gap). The same holds for
a long list item block (`s1_a_long_list_block_reaches_history_while_it_streams`)
and for a table once it closes.

## P1 — the whole path is linear

Two things made a big message quadratic: every delta re-rendered every block
committed so far, and every delta re-rendered the whole open block.

- `Message` keeps the rendered rows **cached per block** (`Block::rows`) and
  starts every walk at `next`, the first block with a row left, so a delta
  costs nothing for what is already out. A block whose rows are out is dropped
  from the cache (`committed = DELIVERED`).
- The open block costs only what is left of it (`stable_cut` + `piece_rows` +
  `open_rows`), i.e. at most `LIVE_KEEP` complete lines plus the partial line.

Measured (`tests/review_tui2b.rs`, ratios not wall clocks):

| case | before (round 2 review) | now |
|---|---|---|
| 44 KB mixed markdown, 4 KB deltas | — | 37 ms |
| 1044 KB mixed markdown, 4 KB deltas | 4.7 s at 104 KB, "minutes" at 1 MB | 687 ms (23.3× the 44 KB case for 23.7× the bytes: linear) |
| 1 MB fence, 64-byte deltas | — | 672 ms total, worst single delta 78 ms |

Two regressions pin it: `a_megabyte_of_mixed_markdown_scales_linearly` (ratio
of the 100 KB to the 1 MB case, allowance 3× linear plus 250 ms) and
`a_one_megabyte_fence_streams_row_by_row_in_bounded_time` (total < 20 s, worst
delta < 500 ms). The older `a_big_multi_paragraph_message_is_not_quadratic` and
`one_megabyte_single_block_per_delta_cost_does_not_explode` still pass
unchanged.

## R5 — the hint names the key that works

- `activity.rs`: the `rate limited` row says `esc stops`, like every other
  working state (`say stop` was a lie in this pane: typed `stop` is a `say` the
  model receives).
- The scrollback line for a wait (`» rate limited (429): …`) is rewritten too
  (`tui::tui_wait_line`), which the round-2 review also caught (it echoed the
  CLI's sentence); a notice that is not a wait is untouched.
- CLI stderr and the `error` event keep STEER-5's `say stop to cancel`, where
  it is true: only the pane's rows name the key.

Verified in the real binary: `rlstop.w1` reads
`⠴ rate limited · 429 · retrying in 2 s · attempt 1 · esc stops` and the facts
row `kelir working · rate limited (429): retrying in 5 s, attempt 1, esc stops ·
2 s left`; Esc then ends the turn (`⏹ 1 step · 0 tok · 4s`, `» stop (Esc)`).

## S3b — the media type comes from the bytes

`attachments::mime_of` now sniffs the header (`png`/`jpg`/`gif`/`webp`) and
`mime_of_format` takes the `data:` media type from that format, with the
extension only as a last-resort fallback when the bytes are unknown. A GIF or
WebP named `x.png` is sent as what it is. `N-chip`: `pasted_image` sniffs too,
so a text file named `fake.png` makes no chip (nothing promises an image
`kelir view` would refuse).

## Nits

- `N-echo` (prompt echo glued to the first prose row) — **this was still real
  in a pane after the first fix, and the unit tests were green**, because the
  spacing helpers asked `scroll.is_empty()` and the event loop drains `scroll`
  after every paint: between two events "nothing committed yet" was true again.
  The decision now reads `App::last_row: Option<bool>`, updated by
  `take_scroll`. Two blocks now always get exactly one blank row, painted or
  not. New test `the_spacing_rules_hold_when_every_event_is_painted` drains
  after every event (the old test never drained, which is why it could not
  see it); the pty replay shows one blank row after `› go` at 80 and 40
  columns. The same fix covers a doubled blank row after a heading
  (`..\n\n..`), which the same drain had been hiding.
- `N-label`: a fence's language label inside a list item is indented with its
  code.
- `N-para`: a loose list item's paragraphs get the blank row CommonMark gives
  them (`Renderer::paragraph_start`, one row, only inside an item and only when
  the previous row is not already blank; a tight list stays tight).
- `N-table8`: a table too wide for the pane at 8/5 columns drops columns instead
  of overflowing by one (`plain_table_rows`); `a_table_at_extreme_widths_does_not_overflow`.
- `N-count`: both waiting rows read the same clock
  (`as_secs_f64().round()`), so `retrying in 6 s` and `6 s left` agree.
- `N-misc`: the empty `rerender_pending` is gone (its comment moved to the
  resize path).
- `N-prompt`: the base prompt is main's STEER-6 text again — the merge had
  reworded it — with the one `kelir view` line inside the 1 KB budget (1015
  bytes), snapshot and rule test updated.

Still open, with the reason, in `docs/DEVIATIONS.md`: a fence nested in a list
item is committed with its item; the two waiting rows still both mention the
wait (`N-count` half fixed); a wide table still truncates a cell with `…`
(`N-table`); `⏸` stays U+23F8.

## Gates

- Windows: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `scripts/gate.sh --linux=<machine>` → `gate: PASS` (nextest 786 passed, 9
  skipped, no retries; doctest ok; Linux fmt/clippy/nextest/doctest ok).
- The markdown + stream suites three times: 34 passed each time.
- `--lib`: 470 tests (was 467: the painted-spacing test, the loose-item
  blank-row test and the wait-line key test).
- `review_tui2b`: 21 passed **with every `#[ignore]` removed** (the six finding
  tests are now regressions), `review_tui2`: 13, `e2e_tui`: 26, `e2e_tui2`: 2.
- The real binary in a pty (`scripts/live/tui2_scn.py`, scripted mock), replayed
  through pyte and **looked at**: a long streamed answer with a fence, a table
  and a nested loose list at 80 and 40 columns (mid-stream and final frame); the
  60-line fence mid-stream at 80 and 40; a 429 wait, Esc during it; a provider
  exhaustion pause; calls-then-answer and interleaved turns. No row repeated, no
  row lost, no row wider than the pane.
- Process check after the drives: no leftover `kelir` from this job (the
  scenario driver kills its mock and its pane by pid).

## What cost time

- The N-echo trap: a unit test that never drains the scrollback cannot see a
  spacing bug that only exists in a real pane. Driving the binary is what found
  it, twice.
- The prompt budget: main's file is 1014 bytes, so any added line needs a trim
  somewhere; keeping main's words and trimming three phrases was the honest
  option, and the retro says which three.
