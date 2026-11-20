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

## Update (2026-11-19 13:50) — rate limit (429) wave and STEER-5

Five concurrent kelir sessions exceeded the OpenCode Go rate limit (HTTP 429, kelir gave up after short
retries). Decisions (maintainer: "fix that, retry with backoff"):
- **STEER-5** (docs/tasks/STEER-5.md): retry with Retry-After / exponential full-jitter backoff, interruptible,
  visible, pause-not-fail on exhaustion, optional client-side concurrency governor. Worktree kelir-wt/steer5, pane
  next to steer4.
- Concurrency capped at **three** kelir workers until STEER-5 lands: M5-A (t10z), steer4 (t113), steer5. **steer3
  parked** (stopped, resumes after STEER-5); steer1fix and steer2 parked (errored on 429, resume later with
  `ctl <session> say continue`, state dirs under kelir-wt/state/<job>).
- Merge order: M5-A, steer4, steer5, steer1fix, steer2, steer3.

## Update (2026-11-19 14:25) — M5-A gated, in review; first retro landed; STEER-6

- M5-A (sources + skills): gates on main green (Windows 419, Linux 424); fresh reviewer pane t116 running.
- Introspective pass on M5-A's transcript (Claude, pane t115) done: docs/retros/m5a.md (commit 8e9c74b): 99.4% cache
  hit, 113 min, 320 steps; top findings: silent 8191-char bash command cut (Windows), design thrash (49% of completion
  tokens), python-heredoc patches over `edit`. -> brief **STEER-6** (docs/tasks/STEER-6.md), runs after STEER-5 (a slot
  and provider capacity), as a kelir-implemented job in its own worktree.
- Order: steer4, steer5 (running) -> M5-A review fixes -> steer6 -> steer1fix -> steer2 -> steer3 -> M5-B -> M6...

- 14:00 M5-A review = PASS WITH FIXES (0 blockers, 11 should-fix, docs/reviews/m5a-review.md); fixes job **m5afix**
  (docs/tasks/M5-A-fixes.md) in kelir-wt/m5afix; running now with steer4 and steer5 (three kelir workers).

## Update (2026-11-19 15:05) — three gaps recorded (from the admin session on the maintainer; I agree with all three)

1. **STEER-7** (docs/tasks/STEER-7.md): socket hardening — refuse foreign job ids/paths, per-session token (retro F8).
2. **STEER-8** (docs/tasks/STEER-8.md): cost from a price table when the provider reports 0, `ctl status` with steps,
   last prompt size, window %, cache ratio, retry state; threshold warnings (retro F9).
3. **M12** (docs/tasks/M12.md): publish preparation (history scrub, first CI run, README/LICENSE/SECURITY,
   dry-run publish). **The push itself is gated on the maintainer's explicit yes** (a decision only he can make):
   asked once at the end of M12, not before.
Order after the current wave: steer4, steer5, m5afix -> steer6 -> steer7 -> steer8 -> steer1fix -> steer2 -> steer3 ->
M5-B -> M6 ... M11 -> M12. STEER-5/6 already cover rate-limit, compaction trigger, design thrash, lying-tool exit codes.

## Update (2026-11-19 16:10) — steer4 merged, M5-A review fixes + steer5 running

- **steer4 merged to main** (9dd351f): PASS WITH FIXES, fixed (steer4fix), gates green on Linux (446) and Windows (439
  with 2 load flakes that also hit main). The user config line `max_steps = 4294967295` can go once the pinned/installed
  binary is the new one (the installed `.cargo/bin/kelir.exe` is still old: it would fall back to 100).
- Introspective retros landed: docs/retros/m5a.md, docs/retros/steer4.md (same top findings: design thrash, 8 KB command cut,
  429 giving up) -> STEER-5 (running), STEER-6 (queued).
- Flaky e2e under load recorded as **STEER-9** (docs/tasks/STEER-9.md); gates now use `cargo test --no-fail-fast`.
- Running: steer5 (t114), m5afix (t119). Merge order next: m5afix, steer5, then steer6, steer7, steer8, steer9, steer1fix, steer2, steer3, M5-B ...

- 16:25 Parked workers closed (they had only read files: no commits, clean worktrees): steer1fix (t110), steer2 (t111),
  steer3 (t112). Their Herdr "needs attention" flags cleared by closing the panes. Worktrees kelir-wt/steer1fix,
  steer2, steer3 kept (clean, branches at the old base): **relaunch fresh** from a rebased/new worktree off current main
  when a slot frees (do not resume; their transcripts are in kelir-wt/state/<job>/ if the retro wants them).
  Running now: steer5 (t114), m5afix (t119), steer6 (t11s).

## Update (2026-11-19 16:40) — TUI polish (maintainer)

New requirement: polish the TUI: clear idle-vs-working, collapsed/de-emphasised tool calls, image paste stored as a file
in the session state dir with a pointer in the prompt (model reads it on demand via `kelir view`), full markdown.
- **STEER-10** (docs/tasks/STEER-10.md) + STEER-3 = one job **tui2**.
- Step 1 (running): fresh Claude design analyst compares opencode and Claude Code runs (screenshots) and writes
  docs/design/TUI-v2.md. Step 2: kelir implements tui2 in kelir-wt/tui2 from that note. Step 3: reviewer with screenshots,
  introspective retro, merge. Parks STEER-3 as a separate job (superseded by tui2).

## Update (2026-11-19 17:00) — self-documentation (maintainer)

**STEER-11** (docs/tasks/STEER-11.md): organised `docs/reference/` of everything configurable (config/CLI/env/verbs/files/
events/providers/troubleshooting + source map), generated where possible and guarded by a drift test, embedded and readable
via `kelir docs`, shipped as a builtin skill `kelir` that the hint (M5-B) and steering rules (M8) can inject. SPEC.md has a
Self-documentation section; AGENTS.md says docs are part of done. Run it right after STEER-5/6 (it needs the env-var registry
and the final config surface), before M5-B.

## Update (2026-11-19 18:20) — M5-A fixes merged; steer5/steer6 FAILED review, fixers running

- **m5afix merged** (e12d3d6). 
- **steer5 (retry/backoff) review = FAIL** (2 blockers: branch predates STEER-4 -> two Paused variants; say --now/model don't
  interrupt a wait; 4 should-fix): fixer **steer5fix** running in kelir-wt/steer5 (brief docs/tasks/STEER-5-fixes.md, pane t12c).
- **steer6 (retro-driven loop) review = FAIL** (1 blocker: cmd.exe long route drops /C; S1 backslash halving on the short Windows
  route; S2 parallel_tool_calls depends on slow model list; S3 reasoning_effort unconditional): fixer **steer6fix** running in
  kelir-wt/steer6 (docs/tasks/STEER-6-fixes.md, pane t12q).
- **tui2** (TUI v2 + markdown) running (pane t11z). Design note docs/design/TUI-v2.md (ea9fc54). **steer11** (self-docs) and
  steer7/8/9, steer1fix, steer2, M5-B, M6.. queued; slots limited to three kelir workers (rate limit until STEER-5 lands).

## Update (2026-11-19 22:55) — build-infra order, steer6 round 2, tui2 in QA

- **BUILD-INFRA** (docs/tasks/BUILD-INFRA.md; work order from the maintainer via the admin session): one worktree
  (kelir-wt/buildinfra, pane t13n), lands before M5-B. The orchestrator's gate script already holds a single-gate lock and caps
  `CARGO_BUILD_JOBS=4`/`RUST_TEST_THREADS=4`; nextest, Linux-dir reuse, lld/profile changes are the worker's job, measured.
- **steer6**: round-2 review PASS WITH FIXES (N5/N6 small): worker **steer6c** (kelir-wt/steer6, pane t13m); merge after it.
- **steer5fix** still running (t12r); **tui2** gates green (Windows 534/0; Linux 513 with one load flake that passes 4/4 alone),
  fresh QA reviewer t13k doing the emulator-render visual review; retro docs/retros/tui2.md landed (8 KB cut again, double
  `cargo test` runs = 37% of wall, no compaction, a git stash/reset incident lost work).
- Queue after: merge steer6c, steer5fix, tui2; then steer11 (self-docs), steer7, steer8, steer9, steer1fix, steer2, M5-B, M6.

## Update (2026-11-20 05:10) — steer6 and steer5 merged

- **steer6 merged** (48d32df) and **steer5 merged** (8170721): retry/backoff on 429 and friends, interruptible waits, paused-not-failed,
  governor; script-file shell route (no 8 KB cut), reasoning_effort gate, multi-edit, compact_at_tokens, prompt lines.
- Pinned binary refreshed to kelir-s4.exe (has both); all launchers use it, so new workers ride out 429s and no longer lose
  heredocs to the 8 KB cut.
- Running: buildinfra (t13n), tui2fix (t13t). Next to launch when a slot frees: steer11 (self-docs; after tui2 merges because it
  extends docs/reference created by tui2), steer7, steer8, steer9, steer1fix, steer2, then M5-B, M6.

## Update (2026-11-20 06:40) — buildinfra merged (gate green), under review; workers

- **buildinfra merged to main** (566379c) pending review; the new `bash scripts/gate.sh --linux` passed on the merged main:
  fmt, clippy, nextest 641/641 (2 skipped), doctests, Linux. Use it instead of my gates.sh from now on. Reviewer pane running
  (safety of the remote `prune`, lock under concurrency, nextest retries, lld/profile, doc numbers).
- Known issue to fix before M12: `scripts/gate.sh` defaults the Linux machine name to a private host (repo must stay public-clean).
- Running: steer2 (t144), steer7 (t145), tui2m (t146: merging main into tui2, conflicts).

## Update (2026-11-20 08:10) — steer2 merged; user config migrated

- **steer2 merged** (bac8851). On the maintainer's machine: `%APPDATA%\kelir\config.toml` migrated (`[providers.kelir]` -> `[providers.openrouter]`,
  `key_env = "KELIR_OPENROUTER_API_KEY"`; backup `config.toml.bak-pre-steer2`); the `max_steps` stopgap line **deleted** (unlimited is the
  default since STEER-4); the copied `KELIR_GO_API_KEY` removed from the user environment and from the private `.env` (kelir reads
  opencode's store directly; `kelir doctor` verified: expires 2026-11-24, read in 383 ms). The maintainer's own installed `~/.cargo/bin/kelir.exe` was reinstalled from main (bca5a61) on 2026-11-20 and verified (kelir doctor ok,
  no step cap by default): nothing to do. (Earlier note saying it was an old build is obsolete.)
- Pinned binary now kelir-s5.exe (steer2, steer5, steer6, steer4).
- Running: steer7 (t145), buildinfrafix (t148), tui2fix2 (t14d). Next: steer1fix/steer8/steer9/steer11 as slots free; then merges of buildinfrafix, tui2.

## Update (2026-11-20 10:20) — tui2, steer1fix, buildinfra merged; main gate 863 green

- Merged: **tui2** (8e2a6b6, TUI v2), **steer1fix** (29f5c16), **buildinfra** (+fixes, +core-dump fix). Combined main passes `scripts/gate.sh --linux`
  (863 tests, Linux ok). The maintainer's installed binary was reinstalled earlier (before TUI v2): reinstall from main HEAD to get the new TUI.
- Linux box: 25 stale verify dirs removed (~90 GB); the new `remote-check.sh prune` only prunes marker-bearing dirs (new ones).
- Running: **steer7fix** (socket hardening, branch job/steer7fix; first review FAIL), **steer12** (measured prompt change, from the admin route's
  harness learning), launching **steer11** (self-docs; docs/reference now exists). Then steer8 (cost/status), steer9 (de-flake), M5-B, M6.

- 2026-11-20 10:50: **tui3** (the maintainer's TUI calm-down pass, branch job/tui3, 1 commit 6d3d8a4, worktree kelir-wt/tui3): handed over via a
  message file; gate PASS per the author (Windows 864, Linux ok). In review (fresh reviewer, emulator-render method); merge after PASS.
  steer12 (measured prompt change) is done and awaiting gate/review; steer7fix and steer11 running.

## Directive (maintainer, relayed 2026-11-20 ~13:00): agent work moves to Claude Code
The OpenCode Go subscription is running low. **All new implementer, fixer and reviewer panes run on Claude Code** (`claude
--dangerously-skip-permissions "<brief>"`, fresh pane/session per pass) through `kelir-scratch/launch-claude.sh <workdir> <prompt-file>`.
No new `--provider opencode-go` kelir runs (if a kelir run is needed for a gate, use `--provider openrouter --model
anthropic/claude-sonnet-4.5`). Already-running opencode-go kelir workers (steer7fix, steer11, tui3fix at this time) finish their current pass;
nothing new starts on them. Discipline unchanged (briefs, gates, fresh reviewer, PROGRESS/QUEUE), keep driving to M11. Note: dogfooding kelir as
implementer pauses; the retro/introspective passes keep reading the transcripts already recorded.

- 2026-11-20 13:30: **steer12 merged** (89e23d6). The AFTER measurement needs kelir sessions on the new prompt; agent work moved to Claude Code (directive), so it is
  deferred until kelir runs again (a gate-run/dogfood session on the OpenRouter fallback can supply a data point). steer7: merge resolved (steer7m, gate 897 green),
  adversarial re-review running (Claude). tui3fix and steer11 (kelir/opencode-go) finish their current pass.

- 2026-11-20 14:10: **tui3 merged** (68943bb). Running (Claude): steer7fix2 (STEER-7 round 3: authenticated status files, t15g), steer11 review (t15j). Waiting
  to merge: steer11 (gate 903 green, review running). Next after those: steer8 (cost/status), steer9 (de-flake), M5-B (Herdr line, hint, memory, attachments,
  observe), M6 MCP, M7 children, M8 gates, M9, M10, M11, M12 (publish prep, push needs the maintainer's yes).

## Update (2026-11-20 14:40) — status (all new agent work on Claude Code)

- Landed today: STEER-4, M5-A (+fixes), STEER-6, STEER-5, STEER-2, STEER-1 fixes, BUILD-INFRA (+fixes, +core dumps), TUI v2, tui3, STEER-12, STEER-11. Main gate green.
- **STEER-7** (socket hardening) is on its 4th pass: reviews FAIL x3 (impersonation, then forged status files, then key disclosure via bg-spawn/bg-claim);
  steer7fix3 running (Claude) with the principle "no secret ever leaves the session or the shim".
- **M6 (MCP as CLI)** running (Claude implementer, kelir-wt/m6).
- Pending: steer8 (cost/status), steer9 (de-flake e2e; flakes now named in every gate summary), M5-B (Herdr line, hint, memory, attachments, observe), M7 (children as CLI),
  M8, M9, M10, M11, M12 (publish prep; push needs the maintainer's yes).
- The steer12 AFTER metrics need kelir runs on the new prompt (kelir implementer runs are paused by the directive): collect one data point from a kelir gate/dogfood run.

- 2026-11-20 16:40: **STEER-7 merged** after four review rounds (accepted with the documented same-user boundary). M6 must now merge main and put the MCP broker requests under the
  session token (the m6fix worker was told to keep the request path in one function). Next: M6 review fixes (m6fix running), then merge M6 + token integration; steer8, steer9, M5-B, M7...
