# tui2 review (STEER-10 + STEER-3), reviewer/tester

Range `ea9fc54..20cefea` on `job/tui2`. Reviewer: fresh Claude Code session, 2026-11-19.
Method: diff read against `docs/tasks/STEER-10.md`, `STEER-3.md`, `docs/design/TUI-v2.md`, `docs/DEVIATIONS.md`;
gates on Windows and Linux; the real binary driven in a pty (ConPTY on Windows, openpty on Linux) with a
scripted mock and with the real cheap model (OpenRouter `deepseek/deepseek-v4.1-flash-1017`, three short runs,
about $0.005 in total) from a Herdr pane; the raw ANSI stream replayed through pyte and rasterised to PNG at
120/110/100/80/60/40/20/8/2 columns, dark and light, `NO_COLOR`, `KELIR_ASCII`, with resize storms. No desktop
screenshots were taken. Images are under `kelir-scratch/tui2-review/img` (not committed).

## Verdict: FAIL (2 blockers, 8 should-fix, 7 nits)

The markdown renderer, the state row while working, the dim one-line tool calls and the `kelir view` byte-stable
tail image are good and mostly better than the design note asks. Two defects break promises the brief makes
outright and show up in the first live run: the scrollback history is **out of order** (tool lines land after the
prose that followed them), and **Backspace does not remove an image chip**. The vision check the brief requires
does not exist, and the model has no way to learn that `kelir view` exists. Fix the blockers and the should-fix
items in the "image flow" and "state row" groups, then re-review.

## Gates (run by me)

| gate | Windows | Linux (the maintainer, `kelir-verify-tui2-rev`) |
|---|---|---|
| `cargo fmt --check` | pass | pass |
| `cargo clippy --all-targets -- -D warnings` | pass | pass |
| `cargo test` | all suites green: 360 lib, e2e 16, e2e_m3 11, e2e_m4 10, e2e_m5 6, e2e_steer4 6, e2e_tui 23, e2e_tui2 2, e2e_verbs 23, review_m0 8, m1 5, m2 2, m3 11, m4 19, m4b 7, m5a 10 (+10 ignored), steer1 6 (+2 ignored), steer4 9 | first run: `review_m4b::b2_a_promoted_exec_tree...` failed once (load flake: the same test passed twice alone, 7/7 each, and in a later `cargo test --no-fail-fast`: 365 lib + every suite green) |
| release binary | 6.91 MB (budget 20 MB; +0.49 MB markdown delta from the notes, budget 0.7) | |
| idle RSS / CPU | 9.2 MB working set, 0.05 s CPU over ~10 s idle (budget 30 MB); an idle session wrote 0 bytes between the frame after start and `/quit` | |

My own tests: `tests/review_tui2.rs`: 13 tests; 4 run by default and pass (CJK table rows equal width, any two-way
split of a corpus equals one pass, a 500 KB message in bounded time, `kelir view` on 12 awkward inputs does not
panic), **9 are `#[ignore = "FINDING ..."]`** and fail when run with `--ignored` (they prove the findings below, in
the style of `review_steer1.rs`). Nothing in `src/` was touched.

## Findings

| id | sev | where | finding and failing scenario |
|---|---|---|---|
| B1 | **blocker** | `src/tui.rs:787-795` (`UiEvent::Assistant`: `commit_live` runs before the group is closed) | **Tool lines are written to the scrollback after the prose that followed them.** Calls then an answer is the commonest turn shape. Live run (real model): `git --version`, `python --version`, then `echo hello`, then the answer: the transcript reads `git`, `python`, **`All three ran successfully...`**, then `$ echo hello`. Mock run: `edit hello.txt` and `$ sleep 6` appear under the answer that came after them; a `kelir view` turn shows `$ "$KELIR_BIN" view ...` under the model's description. History lies about what happened first. Tests: `tool_lines_land_in_the_scrollback_before_the_prose_that_followed_them`, `a_folded_group_precedes_multi_block_prose`. Fix: close the group (and commit it) on the first `Delta`/`Assistant` before any prose row is queued. |
| B2 | **blocker** | `src/tui.rs:1240` (`translate`: `Backspace` -> `Key::Backspace`); `Key::RemoveChip` (`src/editor.rs:34,309`) is produced by no key | **Backspace does not remove an image chip** (STEER-10 req. 3, `docs/reference/tui.md` "Backspace on an image chip removes the whole chip and deletes its file"). On Linux with a real bracketed paste: `› [image 1]`, Backspace x1 eats the trailing space, x2 repaints `[image 1`, x3 `[image`, x5 `[imag`: the label is erased one character at a time, the chip stays registered, the attachment file stays on disk. The unit test only drives the synthetic `Key::RemoveChip`. Test: `backspace_removes_a_pasted_image_chip_whole` (3 Backspaces after a paste: `chips().len()` is still 1). |
| S1 | should-fix | `src/tui.rs:560-575` (`live_view`), `:643-661` (`trim_pending`) | **Rows vanish while a later block streams, and a long block is a 3-row ticker.** While a fenced block streams, the previous block's last <=3 rows are in neither the scrollback nor the live area (the intro paragraph disappears), and rows of the tail block that scroll off the 3-row window are only committed when the block closes. A 60-line code block shows `l46 / l47 / l48` and nothing else for 2 s, then lands in one burst. The design note intended tail-only live rows but not hiding committed-pending rows. Test: `no_row_is_hidden_while_a_later_block_streams`. Fix: commit the pending rows as soon as a later block starts; consider committing tail-block rows beyond the window as they stop changing. |
| S2 | should-fix | `src/session.rs:1765-1792` (`check_view`), `src/models.rs:81` (`vision_for` has no caller) | **`kelir view` never checks the model's vision flag** (brief: "Without vision support in the model the CLI says so"; the doc comment at `session.rs:124` says it does). A model whose metadata says `capabilities.vision:false` is sent the image anyway. Also, `models::parse` only reads `capabilities.vision` or `supported_parameters` containing `vision|image`; OpenRouter signals images through `architecture.input_modalities`, so wiring the check as is would wrongly refuse vision models. Test: `kelir_view_refuses_when_the_model_cannot_take_images`. |
| S3 | should-fix | `src/attachments.rs:138` (`mime_of` by extension), `src/session.rs:1765` | **A file that only has an image extension is queued as an image.** `echo 'not a png' > fake.png; kelir view fake.png` and a 0-byte `empty.png` both answer `image queued` (rc 0) and become `source` events; the provider's 400 on the base64 garbage then repeats on every later request because the event stays in the log (the conversation is poisoned). The header parser `attachments::dimensions` already exists: refuse when it returns `None`. Test: `kelir_view_refuses_a_file_that_is_not_really_an_image`. |
| S4 | should-fix | `src/prompt.md` (unchanged), pointer text `attachments.rs:187` | **The model is never told `kelir view` exists.** Neither the system prompt nor the `[image: attachments/1-x.png, 200x120, 726 B]` pointer mentions it. Live run: the prompt carried the pointer and a question; the model looked for `attachments/` in the cwd (it lives in the session dir), then spent 7 steps guessing the picture with Python/PIL and `sed` on a text dump, exactly the baseline failure the design note describes. When the user's prompt names `kelir view attachments/1-shot.png` the flow works end to end with the real model (it described the white circle and the red outline correctly, 2 steps, $0.0002). So the mechanism is sound but undiscoverable. Fix: one line in `prompt.md` and/or in the pointer (`... run: kelir view attachments/1-x.png`). |
| S5 | should-fix | `src/activity.rs:187` (`turn_ended` always -> `Idle`), `src/tui.rs:899` | **The `paused` and `error` rows are never visible.** After a provider 401 the state row reads `● idle · ready` (only the facts row says `kelir error`); after a step-cap pause it reads `● idle · ready` instead of `⏸ paused · step cap 2 reached · say continue`. STEER-10 req. 1 lists both states; the design note shows `✗ error · provider 401 · say to retry`. Test: `the_state_row_keeps_paused_and_error_after_the_turn_ends`. |
| S6 | should-fix | `src/tui.rs:1024` | **`/collapse` toggles the policy**: on a fresh session (collapsed) it switches to `full` and prints `» tool output: full`. A command named collapse must not expand. (Evidence: pty run; the code is a one-liner.) |
| S7 | should-fix | `src/markdown.rs:1050` (`Stream::line_ended`: a blank line always closes the block) | **A list item's continuation paragraph loses its indent.** `- item\n\n  continued paragraph\n\n- next` renders the continuation flush left, as if it were a new paragraph (CommonMark keeps it inside the item); same for a fence inside a loose list item. Models write loose lists and code in items constantly. Streamed == one-shot still holds (it is by construction), so the stream tests cannot see it. Test: `a_loose_ordered_list_keeps_its_numbers_and_item_continuations_stay_indented`. (A loose ordered list does keep its numbers: `1. 2. 3.`.) |
| S8 | should-fix | `docs/DEVIATIONS.md` "Windows bracketed paste arrives as keystrokes" | **On Windows (the maintainer's OS) the chip never appears**: a pasted path is typed as keys, stays visible as raw `C:\...\shot.png` text and is converted to the pointer only at submit, so "chip `[image 1]` in the input box" and its removal are Unix-only. The deviation is recorded and the submit-time rule works (verified), but req. 3's UI half is not met where it is used. Options: coalesce a burst of key events that spells an image path into a chip, or enable VT input on the console. |
| N1 | nit | `src/tui.rs` spacing | No blank row between the prompt echo and the following prose/tool lines, nor between tool lines and the prose after them (design note section 7 spacing rules). Looks cramped but is consistent. |
| N2 | nit | `src/markdown.rs` list indent | At width 5 a 4-deep nested list overflows by 2 cells (`      …` is 7 wide). Test (ignored): `nothing_is_wider_than_the_layout_at_any_width`. Widths >= 10 are clean for the whole corpus; width 1 cannot hold a double-width glyph. |
| N3 | nit | `KELIR_ASCII` | The ASCII set covers the state/table/bullet glyphs but the banner, `›`, `»` and the `·` separators stay Unicode. Fine if the set is only the design's glyph list; say so in the docs. |
| N4 | nit | turn summary | `✓ 1 steps` (plural). The ConPTY drive also shows `kelir done` on the facts row under `● idle` on the state row: harmless but two words for one state. |
| N5 | nit | user event | The `user` event for a prompt with chips has `"attachments": []`; the design note says the list carries the path (the pointer text in `text` is right). |
| N6 | nit | numbering of `kelir view` copies | Copies of different files all get seq `1` (`1-fake.png`, `1-empty.png`, `1-in-sub.png`); names are unique by suffix and content is de-duplicated, but `seq` does not count. |
| N7 | nit | repo hygiene | `docs/PROGRESS.md` and `docs/retros/tui2-impl-notes.md` name the Linux host `the maintainer`; `scripts/scrub-check.sh` flags it (older docs do the same: pre-existing class). |

## Requirement by requirement (STEER-10 / STEER-3)

1. **Progress unmistakable: PASS WITH FIXES (S5).** Idle `● idle · ready` (nothing animates; 0 bytes written when idle),
   `thinking 4s · waiting for the provider · esc stops` (spinner and elapsed tick, 1 Hz text, 10 Hz glyph),
   `writing 2s · ~171 tok`, `running 2 tools 1s · bash: sleep 6 && echo ...`, dim prompt while working, one-line summary
   `✓ 2 steps · 3.6k tok · $0.0024 · 8s` then idle, Esc stops within one frame and the spinner stops (`⏹ 1 steps ... ` then
   idle), idle Ctrl-C answers and double Ctrl-C quits (exit 0), `say` while thinking is queued and echoed. Rate limited:
   known gap, `Retry` is wired but unemitted until STEER-5 (a 429 mock run showed plain `thinking`, retries were instant).
   Paused/error: see S5.
2. **Tool calls collapsed: PASS WITH FIXES (B1, S6).** One dim line per call with the verdict right-aligned, `ran 4 commands · 2.9s`
   fold for 3+, the failing call red with `✗ exit 1 · 43 B · 0.3s` and one dim tail line (`cat: nofile.txt: No such file...`),
   `edit` keeps its own line, `/expand`/Ctrl-O/overlay work and say plainly that a committed line cannot unfold. Under
   `NO_COLOR` the failed line stays distinguishable (the `✗` and the full-weight text next to dim siblings). Ordering: B1.
3. **Paste images: FAIL (B2, S2, S3, S4, S8).** Unix bracketed paste of a plain, quoted or spaced path: chip `[image 1]`,
   copy under `<session>/attachments/1-shot.png`, pointer `[image: attachments/1-shot.png, 200x120, 726 B]` in the
   submitted prompt (verified in the request body and the `user` event), no base64 in the log. `kelir view <path>` from
   `bash`: a `source` event (`kind: attachment`, relative path) at the tail; the next request's messages are a
   **byte-identical prefix** of the previous one plus the tool result plus one `user` message with a single `image_url`
   part (`data:image/png;base64,...`). Verified against the mock on Windows and Linux and live with the real model.
   The binary on PATH (`~/.cargo/bin/kelir`) is the old build and **does not have `view`** (`error: unrecognized
   subcommand 'view'`): use `$KELIR_BIN` or put the worktree build first. Edge cases: missing path, text file, directory,
   >10 MB (`is 10.5 MB (the limit is 10.0 MB)`), empty arg (clap usage, rc 2), no `KELIR_SOCK` (clear message), `..` traversal
   (`no such image`), unicode+space name, absolute `C:\` path, backslash relative path: no panic, clear messages, except
   S3 (fake/empty accepted) and S2 (no vision check).
4. **All markdown: PASS WITH FIXES (S1, S7).** Headings (H1 bold+underline, H2/H3 bold dim, no `#`), bold, italic, strike,
   inline code (yellow), fences (dim gutter, language label, rows clipped with `…`), nested bullets `• ◦ ▪`, ordered lists
   (CommonMark numbering: `1. 2. 10.` renders `1. 2. 3.`; the design said keep source numbers: arguably better), task
   boxes (green `[x]`), block quotes with the bar on every wrapped row, boxed tables (CJK rows verified equal display
   width by unit test; ConPTY re-addressing makes pyte show them skewed, a replay artefact), links (`text` underlined +
   dim `(url)`), rules, HTML as text, emoji/CJK. Real-model answer with every element renders legibly at 110 columns.
   Streaming: unclosed fence renders as code; any two-way split of a 10-document corpus equals the one-shot render
   (that property holds by construction); a 1 MB paragraph and a 1 MB fence stream in 1.8 s each in release (500 KB in
   debug inside the 20 s bound): the open block is re-rendered per frame (O(block)) so very long single blocks are the
   slowest case, acceptable. S1 and S7 above.
5. **Cleanliness: PASS.** Every frame and every `insert_before` is wrapped in `?2026h/l`, pairs balanced (176/176,
   442/442, 403/403 across runs), about 30 fps cap while streaming, nothing while idle. 120/100/80/60/40/20/8/2 columns:
   no panic, rows clipped to width, exit 0 and terminal restored (bracketed paste off, cursor shown). Resize storm
   (12 size changes x2 during a stream, row counts too): no panic, exit 0, restored. Scrollback is not reflowed (documented).
   `NO_COLOR`: zero colour SGR in the stream, attributes and glyphs kept. Colours seen are only ANSI indexes 1-6, no
   bright, RGB or indexed >7. Light-theme render legible (dim stays readable at 55%). Panic restore covered by the existing
   `a_panic_in_the_tui_restores_the_terminal` (passes).

## Live-drive log

Mock scenarios (scripts: `scripts/live/tui2_{server,drive,render,scn}.py`, run with `uv run --script`): `md100/80/60/40`
(every markdown element, streamed in 9-char chunks, snapshots every 0.5 s), `fence` (60-line fence), `tools100/60/40`
(5 parallel calls with one failure, 2 calls, `edit`, a 6 s sleep, `/expand`, `Ctrl-O`, `/collapse`), `err401`, `rl429`,
`esc`, `think`, `ctrlc`, `pause`, `img1` (paste -> pointer -> `kelir view` -> request bodies), `storm`, `tiny20/8/2`,
`md80nc`, `md80ascii`, `idle`; Linux: `imgl*` (real bracketed paste, chip, Backspace, quoted path, long text).
Real model (Herdr pane `live-tui2*`, key sourced from the env file inside the pane, never printed): `live_md`
(markdown-heavy answer: rendered correctly, 3 steps, $0.0013, B1 visible), `live_tools` (3 calls then answer: B1 visible,
$0.0005), `live_img` (pointer prompt, model never finds `kelir view`: S4, $0.0024), `live_img2` (prompt names
`kelir view`: works, correct description, $0.0002). Process check after the runs: no leftover `kelir`/`bash`/`python`
from my runs (the `kelir-s2` processes belong to other panes and were not touched).
The first version of my replay renderer mis-rendered `SGR 22` (made list text look dim) and my driver crashed in pyte
on 8-column wide glyphs once; both were mine, fixed in the scripts, and the findings above were re-checked from the raw
byte stream, not from the pictures.

## Design-note comparison

Better than the note: `/expand` always appends and `/collapse` is the toggle (note had one command doing both: but see
S6, the toggle's name is wrong); the retry state is wired to a countdown; `[x]` task boxes; the foot print (+0.49 MB).
Deviations that are fine: fixed 8-row viewport (ratatui 0.30 cannot resize an inline viewport), burst-only frame cap,
estimated `~N tok`, clipboard Ctrl-V stub, ordered-list renumbering. Not as designed: spacing rules (N1), the table
that streams as a bordered partial table in the live rows instead of plain pipe rows (cosmetic, it works), `attachments[]`
(N5), `paused`/`error` rows (S5), vision check (S2), `kelir view` discoverability (S4), Windows chip (S8).

## Spec items not covered by tests

Chip removal by a real key, state after a turn that ends in error/pause, tool-line ordering relative to prose, `kelir view`
with a text-only model and with a fake image, list-item continuation: now covered by the ignored tests above. Still
uncovered: clipboard Ctrl-V (stub by design), the rate-limit countdown end to end (STEER-5), Windows Terminal narrowing
ghost rows (needs a real terminal; ConPTY replay cannot show it).
