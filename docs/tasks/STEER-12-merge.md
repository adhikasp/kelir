# STEER-12: merge main into the branch (prompt conflict)

Your worktree kelir-wt/steer12 (branch job/steer12). `git merge main` conflicts in `src/prompt.md`, `tests/fixtures/prompt.md.snapshot` and
`docs/PROGRESS.md`. Main's prompt gained lines since you branched (TUI v2: the `kelir view <path>` line so the model knows images can be viewed;
STEER-6 lines; STEER-2/5 changes, none removed). Resolve so that the final base prompt keeps EVERY behaviour line from both sides, stays
<= 1 KB (tighten wording, never drop a rule; the word `Directories:` must stay, a STEER-6 test cuts the system message there), is
deterministic, and the byte snapshot + the size test + the STEER-12 pinned-prompt test are updated deliberately. Re-run the metrics self-test.
Then gates (`bash scripts/gate.sh --linux=<machine from your prompt>`), commit `steer12m: ...`, and final answer "steer12m ready: <commit>".
