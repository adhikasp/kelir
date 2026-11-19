# Work queue (orchestrator's continuity file)

Order of work, one writer on `src/` at a time. Update as items move.

1. M4 fixes round 2 + repo scrub (docs/tasks/M4-fixes2.md): in flight.
2. No-flash tester verdict (docs/reviews/NOFLASH.md): in flight; findings feed item 1.
3. STEER-1 review (docs/tasks/STEER-1.md): model defaults + provider model metadata, fresh reviewer.
4. STEER-2 (two providers, opencode credential read directly): first job for kelir-as-implementer.
5. STEER-3 (markdown in the TUI): kelir-as-implementer.
6. M5 sources/skills/hint/observe, M6 MCP, M7 spawn/brief, M8 gates, M9 jobs/sessions,
   M10 Telegram, M11 attach: each with implementer -> reviewer -> retro (3 passes).

Per-job loop (user's rule, 2026-11-19): kelir implements (OpenCode Go provider, default model),
a fresh Claude Code reviewer/tester reviews, then a fresh Claude Code agent reads kelir's
transcript (events.jsonl) and proposes improvements to the prompt/loop (docs/retros/<job>.md).
Fresh agent for every pass; the orchestrator carries continuity; delete opencode sessions and
retire panes after each pass.

## State when the orchestrator stopped (2026-11-19, usage limit)

- M0-M3 closed (reviewed, fixed, verified Windows + Linux). M4 reviewed twice; round-2 fixes
  (docs/tasks/M4-fixes2.md: G1-G4 + repo scrub) were in flight in the main tree, UNCOMMITTED when I stopped.
  Verify with `cargo test` (run twice) + `bash scripts/remote-check.sh <linux-machine>` + `scripts/live/flash_matrix2.sh`
  before committing as "M4: fixes 2"; the committed HEAD still flashes console windows (docs/reviews/NOFLASH.md).
- Not started: STEER-1 review, STEER-2, STEER-3, M5-M11. The repo is local only (no remote, nothing pushed).
- Panes: implementer t77 in the kelir-build Herdr tab; delete its opencode session after it finishes
  (`opencode session list` / `delete`). Keep `C:/Users/the maintainer/kelir-scratch/live.env` (OpenRouter key, outside repo).
