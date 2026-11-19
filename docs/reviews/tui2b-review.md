# tui2b review (round 2 of STEER-10, after tui2fix and the merges of main), reviewer/tester

Range `20cefea..d8cd529` on `job/tui2` (tui2fix, steer5m/tui2m merges of main at 997a833). Fresh Claude Code session,
2026-11-20. Method: diff and docs read; gates on Windows and Linux; the real binary (release, 7.18 MB) driven in a
pty (ConPTY on Windows, with the scripted mock) with the raw ANSI stream replayed through pyte and rasterised to PNG
at 100/60/40/30/20 columns, dark and light, `NO_COLOR`, `KELIR_ASCII`, resize storm; one real-model session
(OpenRouter `deepseek/deepseek-v4.1-flash-1017`, 3 turns, 9 steps, about $0.003, started from a Herdr pane
`live-tui2b` with the env file sourced inside the pane, never printed). No desktop screenshots; images are under
`kelir-scratch/tui2b-review/img` and are not committed.

## Verdict: PASS WITH FIXES (0 blockers, 4 should-fix, 11 nits)

Both round-1 blockers are fixed and verified in the real binary (B1 order, B2 chip removal, on Windows where it
matters), as are S2, S4, S5, S6, S7, S8 and nits N4-N6. The merge with STEER-5/STEER-6 works: `rate limited` with a
live countdown and attempt number, `paused` for the step cap and for provider exhaustion, Esc during a wait, a 943-byte
prompt carrying both sides' lines. The real-model session reads well and matches the design note.
Two round-1 items are **not** really done and one new problem came with the block-by-block commit:
**S1 is half fixed** (and `docs/reference/tui.md` claims the other half), a big message is now **quadratic**, and the
state row tells TUI users to *say stop* (which sends the model the word "stop"). Fix those, then it is a clean PASS.

## Gates (run by me)

| gate | Windows | Linux (the maintainer, work dir `kelir-verify-tui2b-rev`) |
|---|---|---|
| `bash scripts/gate.sh` | fmt ok, clippy `-D warnings` ok, nextest **757 passed, 2 skipped, 0 retried**, doctest ok: `gate: PASS` (108 s) | `remote-check.sh the maintainer kelir-verify-tui2b-rev`: rc=0, FMT-OK, CLIPPY-OK, nextest **757 passed, 2 skipped**, DOCTEST-OK |
| release binary | 7.18 MB (budget 20 MB; was 6.91 MB at round 1, the rest is STEER-5/6) | |
| idle RSS | not re-measured (round 1: 9.2 MB; idle session wrote 0 bytes); no idle path changed | |

My tests: `tests/review_tui2b.rs`, 19 tests: **13 run by default and pass** (B1 interleaved / mid-paragraph / parallel /
pause order, S5 row persistence, STEER-5 countdown / slot wait / exhaustion, S6 idempotence, S7 nested loose lists at 80
and 40 columns, S7 streamed == one-shot at every split, narrow tables, a 1 MB single block in 0.5 s), **6 are
`#[ignore = "FINDING ..."]`** and fail when run with `--ignored` (listed in the findings). `cargo fmt --check` and clippy
are clean with the file in place. Nothing in `src/` was touched.

## Round-1 findings, re-verified

| id | status | evidence |
|---|---|---|
| B1 tool lines after later prose | **fixed** | `ord100/60/40` pty runs (calls->answer, answer->calls, prose / call / prose / call / prose, with a paragraph split by a call): every history row is in event order (`ord100.t3.png`). Real model, 3 sequential calls then answer, and calls then table: in order (`live3.t1/t2`). Unit: `b1_*` x4 |
| B2 Backspace on a chip | **fixed** | Windows ConPTY (keystroke burst): `[image 1]` appears at paste time, one Backspace leaves `›` and the file is gone (`imgb.chip/afterbs`); Linux covered by the e2e in the gate |
| S1 rows vanish / 3-row ticker | **half fixed** | the previous block no longer vanishes (the intro paragraph is committed when the fence opens), but the open block is still a 3-row ticker: see **S1b** |
| S2 vision check | **fixed** | `kelir_view_refuses_when_the_model_cannot_take_images` (now un-ignored) passes; silence is "try it"; `input_modalities` parsed |
| S3 magic number | **fixed for `kelir view`** (fake and empty refused before a `source` event); see S3b and N-chip |
| S4 `kelir view` unknown to the model | **fixed, verified live**: the pointer says `— run: kelir view attachments/1-shot.png`; the real model ran exactly that on the first try and described the image correctly (2 steps, $0.0005; round 1 it took 7 steps and failed) |
| S5 paused / error rows | **fixed**: `⏸ paused · step cap 2 reached · say continue`, `⏸ paused · provider unavailable after 2 attempts, 356 ms: provider error (503) · say continue`, `✗ error` stays until the next turn (`pause2`, `exhaust`; unit `s5_*`) |
| S6 `/collapse` toggled | **fixed**, idempotent both ways (`s6_*`) |
| S7 loose list continuation indent | **fixed** at 100/60/40/30 columns, including nested continuation, a fence in an item, and every split point of a streamed list (`mds7_*`; `s7_*`). Two cosmetic leftovers: N-label, N-para |
| S8 Windows chip at paste time | **fixed** (typed or burst path becomes a chip before Enter; quoted path with spaces too) |
| N1 spacing | mostly: one blank row between blocks, never two; the prompt echo is still glued to the first prose row (`› two` / `Now I will run two more.`), nit N-echo |
| N2 prefix wider than the pane | fixed for the round-1 corpus at every width >= 2; one new case, N-table8 |
| N3 / N4 / N5 / N6 | fixed (`1 step`; `attachments[]` carries `attachments/1-my-pic.png`; copies numbered 1, 2) |
| N7 hostnames | `docs/retros/tui2m-impl-notes.md` and `docs/PROGRESS.md` still name `the maintainer`, the same class as before (pre-existing in older docs) |

## Findings

| id | sev | where | finding and failing scenario |
|---|---|---|---|
| S1b | should-fix | `src/tui.rs:587-607` (`settled_rows` commits only *closed* blocks), `src/tui.rs:611-627` (`live_rows` keeps `height` rows); docs: `docs/reference/tui.md:125-129`, `docs/DEVIATIONS.md:982-991` | **A long block that is still open is a 3-row ticker, and its earlier rows are in neither the scrollback nor the live rows until it closes.** Real binary, a 60-line rust fence at 100x30: mid-stream the screen shows `value_36 / value_37 / value_38` and nothing else of the code (`fence.s02.png`); the 60 rows land in one burst at the closing fence (`fence.done.png`). Unit probe after `Delta` x N: `i=3 live=["│ line-01","│ line-02","│ line-03"]` with `scroll=[]`: `line-00` and the language label are gone. The same holds for a 40-item list (one block until a blank line) and a long table. The docs say the opposite ("a 60-line code block scrolls into history as it arrives instead of becoming a three-row ticker"); the round-1 brief allowed "commit finished blocks as they close", which this does, but then the doc claim and the implementer's note ("60 rows, each once, in history mid-stream") are false. Tests (ignored): `s1_no_row_vanishes_and_none_repeats_while_a_fence_streams`, `s1_a_long_list_block_reaches_history_while_it_streams`. Fix: for a fence/list/table, commit the rows that cannot change (all but the last row, or all but the open item) as they appear, or at least say in the docs that it is a ticker. |
| P1 | should-fix | `src/tui.rs:587-607` `settled_rows` (called from `commit_settled`, `src/tui.rs:828`, on every delta) | **Committing block by block made a streamed message quadratic.** Every delta walks *all* blocks and calls `markdown::render_block` twice per block (`rows_after` + `total`), delivered or not. Release build, 24-byte deltas of short paragraphs: 8 KB 36 ms, 26 KB 0.32 s, **104 KB 4.7 s** (worst single delta 14 ms and growing); with 4 KB chunks 26 KB 0.46 s -> 104 KB 5.8 s; a **1 MB** message of ordinary paragraphs ran for more than 8 minutes before I killed it (my own test process, by exact PID). Round 1 streamed 1 MB in 1.8 s. Not visible below about 30 KB, but the brief asked for a 1 MB message and a long tool-output dump rendered as markdown gets there. A single 1 MB block is fine (0.5 s). Test (ignored): `a_big_multi_paragraph_message_is_not_quadratic`. Fix: remember the index of the first undelivered block and start there. |
| R5 | should-fix | `src/activity.rs:322` (`parts.push("say stop")`), `docs/reference/tui.md:47` | **In the TUI the `rate limited` row says `say stop`, and typing `stop` does not stop.** Plain text is a `say`: in `rlsay` I typed `stop` during a 5 s wait and the next request to the mock carried `[user "hi", user "stop"]`; the turn carried on and answered. What cancels is Esc (state `⏹ 1 step · 0 tok · 4s`, then idle, verified in `rlstop.stopped.png`) or `/stop`. Every other working state says `esc stops`; this one (and the facts row, which echoes STEER-5's sentence) points at the wrong key. Fix: the state row says `esc stops` (the STEER-5 sentence can stay for the CLI/NDJSON). |
| S3b | should-fix | `src/fold.rs:212` (`mime_of(&file)`), `src/attachments.rs:176` | **The `data:` media type still comes from the extension, not from the bytes just sniffed.** A GIF (or a WebP, which browsers save as `.png`) named `x.png` passes `kelir view`'s sniff and is sent as `data:image/png;base64,...`; providers that check the declared type against the bytes reject it, and the `source` event then repeats the 400 on every later request (the same poisoning S3 was about). Not proven against a real provider (OpenAI-style endpoints are lenient), but the format is already known. Test (ignored): `the_media_type_follows_the_bytes_not_the_extension` (sniff says `gif`, `mime_of` says `image/png`). Fix: take the mime from `sniff`. |
| N-chip | nit | `src/tui.rs:1291` (`path_tail` chip at keystroke time) | A text file named `fake.png` typed or pasted becomes `[image 2]` and a pointer `[image: attachments/2-fake.png, 19 B — run: kelir view ...]` (no dimensions). `kelir view` then refuses it, so nothing is poisoned, but the chip promises an image. Sniff at chip time. Observed in `imgb.typed.png` and the request body. |
| N-fold | nit | `src/tui.rs` group closing per step | Calls in consecutive steps with no prose between them (the usual real-model flow: one call per step) are not one group: no `ran 3 commands` fold, and a blank row sits between every two lines (`live3.t1.png`: `$ git --version` / blank / `$ python --version` / blank / `$ echo hello`; also between `echo a` and `echo b` in `pause2.paused.png`). Parallel calls in one step fold and sit tight. STEER-10 req. 2 says "groups of consecutive calls fold ... when finished". |
| N-echo | nit | spacing | The prompt echo is glued to the first row of the reply (`› two` / `Now I will run two more.`); there is a blank row before the echo but none after. |
| N-label | nit | `src/markdown.rs:705-710` | A fence's language label inside a list item is flush left while its code is indented (`rust` at column 0, `│ let x = 1;` at column 3; `mds7_60.done.png`). Test (ignored): `a_fence_label_in_a_list_item_is_indented_with_its_code`. |
| N-para | nit | `src/markdown.rs` (`pending_prefix`) | The paragraphs of a loose list item are separated by no blank row, so `First item of the list` / `Continuation paragraph ...` reads as one wrapped paragraph. Indent is right (S7); the vertical gap CommonMark's loose list implies is missing. |
| N-table | nit | `src/markdown.rs:958-1141` | Tables truncate cells with `…` instead of wrapping at narrow widths (`Descript…`, `a rather…` at 30 columns; `alp… \| a r… \| ok` at 20); data in the scrollback cannot be recovered by widening the pane. |
| N-table8 | nit | same | A 3-column table at width 8 or 5 is one cell wider than the pane (`… \| … \| …` is 9 wide at width 8). Test (ignored): `a_table_at_extreme_widths_does_not_overflow`. Widths >= 10 are clean. |
| N-count | nit | `src/activity.rs` / facts row | The same wait is `retrying in 6 s` on the state row and `5 s left` on the facts row at the same instant (ceil vs floor); and with both rows the wait is said twice. |
| N-prompt | nit | `src/prompt.md` (943 B) | The merge kept every STEER-6 rule but reworded and dropped some of main's wording: "Calls start in the cwd above", "parallel calls work", "do not redesign after each tool result", "Verify before claiming". If the STEER-6 bench measured those exact lines, re-run it; the size test and snapshot are consistent. |
| N-misc | nit | `src/tui.rs:~842` | `rerender_pending` is an empty function with a comment; delete it. `docs/retros/tui2m-impl-notes.md` names `the maintainer` (N7 class). Pyte shows `⏸` as a box in my renderer: the glyph is U+23F8, which some terminals draw double-width as an emoji; I could not check a real terminal. |

## STEER-5 / STEER-6 integration (requested checks)

- **State row `rate limited`** is fed by the real retry events: `⠋ rate limited · 429 · retrying in 6 s · attempt 1 · say stop`
  with the facts row `kelir working · rate limited (429): retrying in 8 s, attempt 1, say stop to cancel · 5 s left`
  (`rlstop.w1.png`); the countdown moves with the clock (`steer5_rate_limited_shows_countdown_and_attempt_and_clears`),
  clears on `RetryingDone`, and a governor slot wait (`status None, attempt 0`) does not read as rate limited.
- **`paused`**: step cap -> `⏸ paused · step cap 2 reached · say continue`; provider exhaustion (503 x2) -> `⏸ paused ·
  provider unavailable after 2 attempts, 356 ms: provider error (503) · say continue`, the spinner and countdown are
  gone, and `continue` retries and succeeds (`exhaust.after.png`). Both leave `kelir paused` on the facts row.
- **Interrupt during a wait**: Esc cancels at once and returns to idle (`rlstop.stopped.png`); typing the word `stop`
  does not (R5).
- **STEER-6 prompt lines**: present in substance, 943 bytes, snapshot and `<= 1024` test pass (N-prompt for wording).

## Markdown, tables, performance

Elements re-checked at 100/60/40/30 columns and light (`mds7_*`, `md60l`): H1/H2, bold/italic/strike, inline code,
nested ordered/bullet/task lists, loose items with continuation paragraphs (nested and with a fence), block quotes,
boxed tables (CJK rows are equal width by the unit test; pyte skews them, a replay artefact as in round 1), rules, a
real-model answer with a table and a fenced block (`live3.t3.png`). Narrow tables: never wider than the pane at widths
>= 10, truncated rather than wrapped (N-table). Colour: only indexed 2/3/6 plus bright-less attributes in the stream;
`NO_COLOR` has zero colour SGR; `KELIR_ASCII` keeps the state words; every frame is wrapped in `?2026h/l` (173/173,
384/384); on exit bracketed paste is off and the cursor shown (storm, 20 columns, live). Resize storm and a 20-column
pane: no panic, exit 0. Performance: P1; a single 1 MB block 0.5 s.

## Live-drive log

Mock (scripts: `scripts/live/tui2_{server,drive,render,scn}.py`, specs from `scripts/live/tui2b_specs.py` and the
round-1 specs): `fence`, `ord100/40`, `pause2`, `exhaust`, `rlstop`, `rlsay`, `mds7_100/60/40/30`, `md60l` (light),
`md80nc`, `md80ascii`, `md40`, `tools100/60`, `storm`, `tiny20`, `imgb` (Windows keystroke burst: plain, quoted-with-space,
text-file `.png`, Backspace, `kelir view` turn). Real model (Herdr pane `live-tui2b`, exit 0): turn 1 three sequential
bash calls, turn 2 an `edit` plus `cat` plus a table and fence answer, turn 3 a typed image path: chip -> pointer ->
`kelir view` -> description of the white circle and red outline. 9 steps, about $0.003, all rows in order. Process
check: no leftover `kelir`, `bash` or `python` from my runs (the `kelir-s4` processes belong to other panes).

## Judgement against `docs/design/TUI-v2.md`

It now looks like the design: dim one-line calls with a right-aligned verdict, the red failing call with its tail, one
state row that is unmistakable (idle `●`, working spinner with elapsed and `esc stops`, `rate limited` with a real
countdown, `⏸ paused` and `✗ error` that stay), a one-line summary, then idle; the prose is the focus; markdown is
complete. What is below the design: streaming a long single block (S1b), the fold for sequential calls (N-fold), the
spacing after the echo (N-echo). One real defect to keep in mind for resilience: the quadratic commit (P1).

## Spec items not covered by tests

Real Windows Terminal narrowing (ConPTY replay cannot show ghost rows), the Windows clipboard Ctrl-V image (documented
stub), `⏸` width in real terminals, a real OpenRouter model that states `input_modalities` without image
(only the parse is tested). The `rate limited` countdown is now covered end to end (unit + pty).
