# tui3 fixes (from docs/reviews/tui3-review.md + its addendum; PASS WITH FIXES, 0 blockers, 4 should-fix, 6 nits)

Worktree kelir-wt/tui3 (branch job/tui3, HEAD 627d882 + review commits). First `git merge main`. Read the review and make the reviewer's
tests (tests/review_tui3*.rs or similar) pass unweakened. The maintainer's calm design stays: facts row without steps/%cache/USD, one folded
checked line for runs of 3+ calls, done turn ends on its prose, no window line in the banner. Fix:
- **F-cancel**: a cancelled/stopped call must not be counted as a success nor folded away: it keeps its own visible line (or the fold says
  "N calls, 1 stopped"); the stop outcome line must stay consistent.
- **F-edit**: `edit` (and any file-changing call) keeps its own line (as in tui2: the diff is what a reader cares about); only read/exec
  shell calls fold. Update docs/reference/tui.md.
- **F-window**: keep the banner free of the normal window line, but the *degraded* window note (401, offline endpoint, default fallback)
  must still reach a person: show it only when degraded, as one dim line (the STEER-1 review N4 reason); and a `/model` swap after a
  fresh-cache startup must be announced when the window changes (initialise `window_model` from the startup answer instead of None). Test
  offline (KELIR_BASE_URL pointing to a dead port) and a swap.
- **F-dev**: record the deviations from STEER-10 reqs 1-2 in docs/DEVIATIONS.md and update docs/design/TUI-v2.md (facts row, summary line,
  tool verdict fields) so the docs match the new design; where do steps/cost/cache now live? (`kelir ctl status` arrives with STEER-8; say so.)
- Nits: fix the cheap ones, list the rest.
Gates: `bash scripts/gate.sh --linux=<machine from your prompt>`; render checks by pty replay as the reviewer did (scripts/live). Commit
`tui3fix: ...` (explicit paths), notes docs/retros/tui3fix-impl-notes.md, final answer "tui3fix ready: <commit>".
