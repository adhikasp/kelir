# Work queue (orchestrator's continuity file)

Order of work, one writer on `src/` at a time. Update as items move.

0. **M5-A: sources + skills** (`docs/tasks/M5-A.md`) — read `AGENTS.md`, `CLAUDE.md` and the skills
   directories at session start, plus the `source` event and the `skills` verb. Promoted ahead of
   everything else (the maintainera, 2026-11-19): every later job gets better once the harness reads the
   repo's own instructions, so kelir itself implements this one.
1. M4 fixes round 2 + repo scrub (docs/tasks/M4-fixes2.md): done, committed as `05d1d16`..`1dc2021`;
   gates and the no-flash re-check re-run on that HEAD by the opencode orchestrator.
2. No-flash tester verdict (docs/reviews/NOFLASH.md): closed — 12/12 scenarios, 0 visible windows,
   0 leftovers on the committed build.
3. STEER-1 review (docs/tasks/STEER-1.md): model defaults + provider model metadata, fresh reviewer
   (in flight when the orchestrator changed hands).
4. STEER-2 (two providers, opencode credential read directly).
5. STEER-3 (markdown in the TUI).
6. M5-B (hint, memory, attachments, `observe`), M6 MCP, M7 spawn/brief, M8 gates, M9 jobs/sessions,
   M10 Telegram, M11 attach: each with implementer -> tester -> retro (3 passes).

Per-job loop (the maintainera, 2026-11-19, revised): **kelir implements** and **kelir tests/reviews** (two
fresh sessions, default model — dogfood), then a fresh **opencode** agent reads kelir's own
trajectory (`events.jsonl`: every tool call, retry, failure, token and cache number) and proposes
improvements to the prompt, the loop and the tool surface in `docs/retros/<job>.md`, landing the
cheap ones behind the gates. Fresh session for every pass; the orchestrator carries continuity;
retire panes after each pass.

## State (2026-11-19 11:10) — orchestrator handed over to opencode

Claude Code hit its usage limit at 10:50 (pane t1x). The orchestrator role moved to the opencode
pane **t9m**; implementer and reviewer stay fresh panes but are now opencode sessions
(`~/kelir-scratch/launch-oc.sh <prompt-file>`, default model), because Claude Code cannot be used.
Everything else is unchanged: one writer on `src/` at a time, one report line to `hc send t9m …`,
a fresh session per pass, panes retired after each pass.

Gates re-run by the new orchestrator on committed HEAD `1dc2021`:

- Windows: `cargo fmt --check` clean, `cargo clippy --all-targets -D warnings` clean, full
  `cargo test` twice, both green (262 unit, 16 e2e, 11 e2e_m3, 10 e2e_m4, 15 e2e_tui, 23
  e2e_verbs, 8 review_m0, 5 review_m1, 2 review_m2, 11 review_m3, 19 review_m4, 7 review_m4b).
- Linux (the maintainer, `scripts/remote-check.sh the maintainer`): fmt, clippy, 267+ unit and every suite green.
- Flash re-check: `flash_matrix2`'s twelve scenarios on the committed build, **0 visible windows,
  0 leftovers** (docs/reviews/NOFLASH.md, "Final commit re-run"). The M4 terminal-window item is
  closed.

Next in flight: STEER-1 review (reviewer pane, fresh opencode session), then STEER-2 implement,
STEER-3 implement, M5-M11. The repo is still local only: no remote, nothing pushed.

## State when the Claude orchestrator stopped (2026-11-19 10:50, usage limit)

- M0-M3 closed (reviewed, fixed, verified Windows + Linux). M4 reviewed twice; round-2 fixes
  (docs/tasks/M4-fixes2.md: G1-G4 + repo scrub) were in flight in the main tree, UNCOMMITTED when I stopped.
  Verify with `cargo test` (run twice) + `bash scripts/remote-check.sh <linux-machine>` + `scripts/live/flash_matrix2.sh`
  before committing as "M4: fixes 2"; the committed HEAD still flashes console windows (docs/reviews/NOFLASH.md).
- Not started: STEER-1 review, STEER-2, STEER-3, M5-M11. The repo is local only (no remote, nothing pushed).
- Panes: implementer t77 in the kelir-build Herdr tab; delete its opencode session after it finishes
  (`opencode session list` / `delete`). Keep `C:/Users/the maintainer/kelir-scratch/live.env` (OpenRouter key, outside repo).

## State (2026-11-19 12:00) — Claude orchestrator resumed (pane t1x)

- Verified committed HEAD gates: Linux 400 tests green; Windows 381/382 (one load-only flake:
  `review_m4::r12_a_graceful_exit_leaves_a_promoted_job_running` "end was refused", passes alone and
  3/3 in its own suite) — harden that test later.
- STEER-1 review (opencode session) = PASS WITH FIXES: S1-S3 + N1-N5, brief docs/tasks/STEER-1-fixes.md.
- First dogfood job running: **kelir (OpenCode Go, default model) implements STEER-1-fixes** in pane
  t10w (tab kelir-build). Pinned binary C:/Users/the maintainer/kelir-scratch/bin/kelir-stable.exe (built at
  94719d2). Next: fresh Claude reviewer, then fresh Claude transcript/retro pass over kelir's events.jsonl
  (docs/retros/STEER-1-fixes.md), then STEER-2, STEER-3, M5-A...
- herdr-app tab.new needs `--space <local space id>` (the focused space may be a Linux machine) and
  `env -u SHELL`.
