# tui2 fixes, round 2 (from docs/reviews/tui2b-review.md; PASS WITH FIXES, 0 blockers, 4 should-fix, 11 nits)

Worktree kelir-wt/tui2 (branch job/tui2, already merged with main). `git merge main` again first (it moved: buildinfra etc.), then fix:
- **S1b + docs**: the open (streaming) block must not be a 3-row ticker: show it properly while streaming (bounded by the live
  area; commit completed rows as they finish) and make docs/reference/tui.md true. **P1 (perf, hard requirement of the brief)**:
  block-by-block commit is quadratic: a 104 KB message takes 4.7 s and 1 MB minutes. Make the whole path linear (incremental parse,
  cache wrapped rows per block, never re-render committed blocks); add a regression test with a time/step budget on 1 MB of
  mixed markdown (assert linear scaling: 100 KB vs 1 MB ratio, not an absolute wall-clock) and fix any other O(n^2) you find.
- **R5**: the rate-limited state row says "say stop" but typed `stop` is sent to the model as a message; Esc works. Make the hint
  truthful ("esc to stop") everywhere, and consider also honouring a typed `/stop`. **S3b**: the data-URL MIME comes from the
  file extension, not the bytes: derive it from the magic number.
- Fix the cheap nits from the review (N-echo prompt glued to the first prose row, N-label/N-para cosmetic leftovers, N-table8);
  list the rest in DEVIATIONS.md.
Gates: `bash scripts/gate.sh --linux=<machine from your prompt>` green; run the markdown/stream suites 3x. Re-drive the real binary
in a pty and LOOK at the replayed rows for a long streamed answer (fence, table, nested list) at 80 and 40 columns before reporting.
Commit `tui2fix2: ...` (explicit paths), notes docs/retros/tui2fix2-impl-notes.md, final answer "tui2fix2 ready: <commit>".
