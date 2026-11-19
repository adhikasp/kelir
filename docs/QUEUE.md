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
6. M5-B (hint, memory, attachments, `observe`), M6 MCP, M7 children-as-CLI/brief (no spawn tool, see below), M8 gates, M9 jobs/sessions,
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

## State (2026-11-19 12:20) — handover recorded, writers sorted, M7 design change

- **Handover**: the opencode orchestrator t9m started the M5-A implementer (kelir, pane t10s) before the
  role came back to Claude (t1x). I had also launched STEER-1-fixes (kelir, pane t10w). One writer on
  `src/` at a time: **t10s (M5-A) continues; t10w was stopped before it wrote anything and STEER-1-fixes is
  requeued behind M5-A** (brief unchanged: docs/tasks/STEER-1-fixes.md). t9m stood down.
- **M7 design change (maintainer, via Telegram)**: no spawn tool. A child is a kelir process the model starts
  from bash (`kelir run --child`, `kelir brief`); `KELIR_ROOT`/`KELIR_PARENT`/`KELIR_DEPTH` ride along with
  `KELIR_SOCK`; the CLI registers `child_started`/`child_settled` with the root's socket (root stays the only
  writer); caps (depth 3, 10 per agent, 16 live per root) are enforced by the CLI. Native tools: bash, edit,
  compact. Delegation is a builtin skill injected at M8. SPEC.md and docs/tasks/M7.md updated.
- Drive mode: continuous through M11, no stops to report; stop only for the usage limit (hand to t9m), a
  missing key, or a decision only the maintainer can make. A watchdog on the Linux box nudges this pane when idle.
- Next after M5-A lands: gates -> fresh reviewer (kelir) -> retro (fresh opencode) -> STEER-1-fixes -> STEER-2 ->
  STEER-3 -> M5-B -> M6 ... M11, recording each landed milestone in docs/PROGRESS.md.

## Rule (maintainer clarification, 2026-11-19): continuous never skips verification

"Drive continuously through M11" means **no pause for approval between milestones, nothing else**.
Every milestone/job ends with, in order: (1) gates on Windows and Linux, (2) a fresh reviewer's verdict
in `docs/reviews/` (fixes re-verified), (3) the retro pass, (4) the PROGRESS row; only then does the
next item start. Failing gate or FAIL verdict = fix first. (Also in AGENTS.md.)

## Update (2026-11-19 12:45) — no step limit by default (maintainer)

`max_steps` unset or 0 = unlimited; knob kept for jobs that want a cap; an explicit cap pauses the turn
("turn paused, say continue"), it does not fail it. SPEC.md, docs/retros/FINDINGS.md and the new brief
docs/tasks/STEER-4.md carry it. Code change touches src/config.rs + src/session.rs, which M5-A is writing
now (one writer rule), so STEER-4 runs **right after M5-A is reviewed**, before STEER-1-fixes. The 400 in
%APPDATA%kelirconfig.toml stays until STEER-4 lands (else M5-A would hit the old 100 cap again).
Order now: M5-A -> review -> retro -> STEER-4 -> STEER-1-fixes -> STEER-2 -> STEER-3 -> M5-B -> M6 ... M11.

- User config stopgap (maintainer, 2026-11-19): `%APPDATA%kelirconfig.toml` has `max_steps = 4294967295` (not 400,
  and not 0: `0` is fatal on the current binary). When STEER-4 lands: **delete that line**, do not set 0.

## Update (2026-11-19 13:05) — Herdr self-awareness (maintainer)

No new tool. **M5 source**: when `HC_TERMINAL_ID` is set (herdr-core also sets `HC_TERMINAL_SOCKET`, `HC_SCOPE`;
`TERM_PROGRAM` is ghostty and unusable) render one frozen line at session_start with pane, space, tab, `where`
from `hc get $HC_TERMINAL_ID --json` (`herdr.space`/`herdr.tab`/`where` labels); fail open if `hc` is missing or
slow; never freeze a live count. **M8 builtin skill `herdr`**: `hc ls --json` (all machines; group `where` by space
prefix to count agents in my space), `hc get/read/send/wait`, `hc machine ls`, rule: never kill another agent's
pane; outside Herdr the line is absent and the skill says so. In SPEC.md (Sources bullet, M5 and M8 rows). Goes into
the M5-B brief (M5-A is already in flight and only covers file sources + skills) and the M8 brief.

## Update (2026-11-19 13:25) — parallel workers, worktrees (maintainer: "increase the progress concurrency")

Concurrency model in AGENTS.md ("Parallel work"). Running now:

| job | where | pane | brief |
|---|---|---|---|
| M5-A (sources, skills) | main tree (finishing) | t10z | docs/tasks/M5-A.md |
| steer1fix | kelir-wt/steer1fix, branch job/steer1fix | t110 | STEER-1-fixes.md |
| steer2 (two providers, opencode credential) | kelir-wt/steer2 | t111 | STEER-2.md |
| steer3 (markdown in the TUI) | kelir-wt/steer3 | t112 | STEER-3.md |
| steer4 (no step limit by default) | kelir-wt/steer4 | t113 | STEER-4.md |

All are kelir-as-implementer (OpenCode Go, default model) with per-job state dirs under
`C:/Users/the maintainer/kelir-wt/state/<job>` (separable transcripts for the introspective pass). Per job: kelir
implements -> orchestrator gates (Win + Linux) -> fresh QA reviewer on the branch -> fresh introspective
agent on the transcript (docs/retros/<job>.md) -> orchestrator merges to main -> gates on main. Merge
order by smallest first, so conflicts stay small: steer4, steer1fix, steer2, steer3. M5-A merges first (it is
already on main). Next wave when slots free: M5-B (Herdr line, hint, memory, attachments, observe), M6 (MCP).
CPU is 6C/12T: at most ~4 concurrent cargo-building workers.
