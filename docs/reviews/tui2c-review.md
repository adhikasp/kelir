# tui2c review (round 3, final short re-review of STEER-10 / TUI v2), reviewer/tester

Range `13a95b9..e66cef6` on `job/tui2c` (tui2fix2 `be08092` plus the merge of main). Fresh Claude Code session, 2026-11-20.
Release binary 7.31 MB (budget 20 MB). Method: real release binary driven in a ConPTY with the scripted mock
(`scripts/live/tui2_scn.py`, replay through pyte to PNG, every image looked at), a timing/memory harness
(`scripts/live/tui2c_perf.py`), and one real-model session (Herdr pane `live-tui2c`, OpenRouter
`deepseek/deepseek-v4.1-flash-1017`, env file sourced inside the pane, 3 turns, 7 steps, about $0.001). Images are in
`kelir-scratch/tui2c/out/img` (not committed). I added only `scripts/live/tui2c_perf.py` and `scripts/live/tui2c_specs.py`;
no `src/` or `tests/` change.

## Verdict: PASS (0 blockers, 0 should-fix, 5 nits)

All four round-2 should-fix items are fixed and verified on the real binary.

## Gates (run by me)

| gate | Windows | Linux (the maintainer, work dir `kelir-verify-tui2c-rev`) |
|---|---|---|
| `bash scripts/gate.sh` / `scripts/remote-check.sh the maintainer kelir-verify-tui2c-rev` | fmt ok, clippy `-D warnings` ok, nextest **850 passed, 5 skipped**, doctest ok: `gate: PASS` | rc=0, FMT-OK, CLIPPY-OK, nextest **850 passed, 5 skipped**, DOCTEST-OK |

CLI: `kelir --help` lists both `view` and `doctor`; `kelir doctor` prints the per-provider key source without the key.

## Round-2 findings re-verified

| id | status | evidence |
|---|---|---|
| P1 quadratic commit | **fixed, linear** | Real binary, 100-column ConPTY, mixed markdown (headings, paragraphs, nested list, fenced code, table, quote), 4 KB deltas: kelir CPU time 100 KB 0.30 s, 500 KB 1.41 s, 1 MB 3.23 s, 4 MB 4.25 s; 64-byte deltas: 0.45 / 2.09 / 4.20 s (100 KB / 500 KB / 1 MB). Wall clock is 5.3 / 29 / 63 s (about 0.06 s per KB at every size) and is bound by ConPTY draining the output, not by kelir (round 2: 104 KB took 4.7 s, 1 MB over 8 minutes). The 4 MB message is fully rendered (end marker on screen), 21 MB RSS afterwards. See N-mem for the transient peak. |
| S1b open block a ticker | **fixed for fences and lists; tables stay a ticker, now documented** | A 60-line rust fence at 80 and 40 columns: mid-stream the screen holds the label and every line so far (`f01..f22`, `f01..f52`), only the partial last line in the live area; the final frame has `f01..f60` once, in order (`s1b_fence_80.mid1`, `s1b_fence_40.mid2`, `.done`). A 25-item nested ordered list (two-digit numbering) at 40 columns: rows reach history while streaming, `10.`..`25.` keep their numbering and nested bullets, none lost or repeated (`s1b_list_40.done`). One long answer with fence + table + list at 80 and 40 columns: no repeated or missing row. Table: while streaming only the last two rows plus the bottom border are visible (`s1b_table_80.mid2`, `s1b_table_40.mid1`); the header and earlier rows come at close. `docs/reference/tui.md:139-140` now says so (N-table-live). |
| R5 "say stop" | **fixed everywhere** | State row `rate limited · 429 · retrying in 1 s · attempt 1 · esc stops`, facts row `kelir working · rate limited (429): retrying in 1 s, attempt 1, esc stops · 516 ms left`, scrollback `» rate limited (429): retrying in 2 s, attempt 1, esc stops`; Esc ends the turn (`» stop (Esc)`, then idle) (`r5_stop2.w1`, `r5_stop.stopped`). Typed `stop` is still a message to the model (as documented); `/stop` also works. |
| S3b mime from bytes | **fixed** | `real_png_named.jpg` (PNG bytes) and `real_jpeg_named.png` (JPEG bytes), typed as paths (chips `[image 1]`, `[image 2]`), then `kelir view` from a tool call: the next request carries `data:image/png;base64,iVBOR...` for the `.jpg` and `data:image/jpeg;base64,/9j/...` for the `.png`. |

## Findings

| id | sev | where | finding |
|---|---|---|---|
| N-mem | nit | transient memory of a huge message | Peak working set during a 1 MB message is about 0.3 MB per KB of input (43 MB at 100 KB, 118-170 MB at 500 KB, 307-333 MB at 1 MB, 360 MB at 4 MB), falling back to 11-21 MB when the turn ends. Linear and bounded, so not P1, but well above the 30 MB idle budget for the burst; probably output queued behind a slow ConPTY (the mock delivers 4 MB in 70 ms). Realistic token-rate streaming will not reach it. Not attributed (no profiler). |
| N-table-live | nit | `src/markdown.rs` (`Shape::Whole`), `docs/reference/tui.md:139` | A table that is still streaming shows only its last two rows; the header and earlier rows appear when it closes (a 30-row table looks like a 2-row ticker for about 4 s). Documented trade-off (column widths need every row), not a docs defect; a plain row-by-row fallback while open would match fences and lists. |
| N-sp | nit | spacing | A list straight after a paragraph gets no blank row when streamed (`List below.` / `1. item`) but one blank row when delivered in one delta (`list_oneshot_80`): streamed and one-shot output differ. A loose item's continuation paragraph is also followed by its nested list with no blank row (`mds7_60.done`). |
| N-fold | nit | tool groups | Calls in separate steps are still not grouped: in turn 2 of the live run a blank row sits between `edit notes.md` and `$ cat notes.md`, while `git --version` / `echo hello` in turn 1 sit tight (`live3c.t3`). Cosmetic and inconsistent. |
| N-table | nit | `src/markdown.rs` | Cells are still truncated with `…` at 60 columns (`a rather long description that must wr…`); known, in DEVIATIONS. |

Non-findings: nested bullets that looked flattened for items 10 and up in my first list runs were my fixture (3-space indent
under a 4-column marker, which CommonMark does not nest); the renderer was right and the corrected fixture nests correctly.
Intermediate pyte frames sometimes show a row missing between a committed row and the live tail (item 11 once); the final
frame always has every row once, and pyte ignores the synchronized-update brackets, so I treat it as a replay artefact.

## Regression pass

Replayed on the release binary: interleaved calls and prose at 60 columns (`ord60.t3`: every row in event order, `2 steps`,
spacing correct, prompt echo followed by a blank row); provider exhaustion (`⏸ paused · provider unavailable after 2
attempts, 453 ms: provider error (503) · say continue`, facts row `kelir paused`); rate-limited row with countdown; loose
nested lists, a fence in a list item, a boxed table, rules at 60 columns (`mds7_60.done`); chips and `kelir view` (S3b run).
Real model, 3 turns: two bash calls then a one-line answer; an `edit` plus `cat` then an answer with a boxed table; `ls` and a
sentence. Summary rows (`√ 3 steps · 4.8k tok · $0.0004 · 3s`), idle row and facts row correct, cache 65% by the end.
Thinking / writing rows as designed (`thinking 0s · waiting for the provider · esc stops`, `writing 4s · ~300 tok · esc stops`).

## Process check

No kelir, bash or mock left from my runs (the drivers kill their mock and kelir by pid; the `python` and `kelir-s5` processes
in `tasklist` started the day before or belong to other panes). Nothing was killed by image name.

## Not covered

Real Windows Terminal (ConPTY replay only), the Windows clipboard Ctrl-V image path, a real provider rejecting a mismatched
data-URL type (S3b is verified at the request, not against a provider), and memory attribution for N-mem.
