# M11 review — fresh QA (attach viewer + notifications)

**Scope:** `C:/Users/the maintainer/kelir-wt/m11rev`, branch `job/m11rev`, diff `d3d3c47..5c73d14`.
Reviewer-only commits: `efe00dc` (tests), `e516012` (ignored failing proof). No `src/` edits by the reviewer.

**Verdict: FAIL — 1 blocker, 4 should-fix.** The attach viewer, notified turn/error/paused paths, docs and footprint are solid; the documented `bg_settled` notify kind does not fire when a background job settles, so the contract in `docs/reference/notify.md` and `docs/DEVIATIONS.md` is unmet.

## Blocker

**[B1] `bg_settled` is advertised but never dispatched.** `src/notify.rs:197-201` lists `bg_settled` as a supported kind and `docs/reference/notify.md:3-4` promises a notify for it; `src/session/children.rs:571-575` receives `child_settled` and `src/session.rs:~3767-3812` (`note_bg`) records `BgSettled`, but the only dispatch site is `src/session.rs:2582-2589`, which handles `turn_done`, `error`, `paused`. A hook configured on `bg_settled` is never run.

Proof: `cargo test --test review_m11 r5_a_settled_bg_job_fires_its_notify_hook -- --ignored --nocapture` (expected pass, actual failure) — no hook marker, log contains no `notify` event, output `bg_settled produced no notify event`. Test: `tests/review_m11.rs:341-384`. This test is `#[ignore]`d only so the rest of the suite can be gated; it is a deterministic product failure, not a flake. Full gate at `efe00dc`: 1 failure in nextest on Windows and the same r5 on Linux; gate FAIL.

Suggested fix: dispatch a `bg_settled` notify at the `BgSettled`/`child_settled` site using the same `notify::fire` path and write exactly one `notify` event; add the ignored test back to the normal set.

## Should-fix

**[S1] Misleading per-job notify documentation.** `docs/reference/notify.md:25-26` says a per-job `notify` is "added to the global hook for that kind"; `docs/DEVIATIONS.md:1815-1818` says M11's per-event table is in `[notify]` and the job `notify` list still runs only at run end in `jobrun.rs`. The reference should say plainly which config block controls which event, not rely on a deviation appendix.

**[S2] `--steer` is ignored without `--follow`.** `src/attach.rs:326-414` only drains steer input in the follow loop; a replay-only `kelir attach --steer <id>` exits without reading stdin and without a hint. `docs/reference/attach.md:29-31` does not state the `--follow` requirement. Low severity because no write happens silently, but it is a surprising flag combination (verified by inspection and by the steer refusal test pattern in `tests/review_m11.rs:250-294`).

**[S3] `--json` Herdr status ambiguity.** `src/herdr.rs:69-73` excludes `--json` from status reporting; `docs/DEVIATIONS.md:608-611` documents that `hc wait` on such a pane cannot see completion. This is documented but still a live-drive usability gap: a machine-driven viewer in JSON mode has no terminal status signal.

**[S4] Linux footprint measured only by the reviewer, not by the implementer.** The brief asked to measure on both OSes; `docs/PROGRESS.md` recorded Linux as unmeasured. Reviewer measured it (below) and it passes, so this is a process gap, not a product defect. Keep the measurement command in the repo.

## What passed (adversarial checks and proving commands)

- **Attach row model / replay:** `cargo test --lib attach::tests::tool_calls_use_the_same_head_model_as_the_tui` — bash `description` uses the same head as the TUI. `tests/review_m11.rs:221-249` replays a finished session and shows outcome/status; live attach pane replayed an existing session with identical rows. `src/tail.rs` unit tests cover torn lines, unterminated lines, `--since` with/without trailing newline, huge lines, file replacement and paged reads (`cargo test --lib tail::`).
- **Read-only:** `tests/e2e_m11.rs:73-98` snapshots the session directory before/after attach; identical. Reviewer's viewer-disconnect test (`tests/review_m11.rs:295-340`) confirms a killed viewer leaves the session alive and writing.
- **Follow / live tail:** viewer pane in the live drive saw new events appear after attach; `hc wait` on the non-JSON session reported `reason:"status", status:"done"` and `hc get` reported `{'state':'done'}`.
- **`--since N`, `--raw`, `--plain`:** `tests/review_m11.rs:182-220` checks documented last-N behaviour; `tests/e2e_m11.rs:99-124` checks raw NDJSON; plain output was exercised by both footprint probes and the live panes.
- **Steer gating:** `tests/review_m11.rs:250-294` refuses every verb except `say`/`stop`, empty `say`, and text that fails M8 inbound gates; no request is sent without `--steer`.
- **Notify:** `tests/e2e_m11.rs:158-190` verifies a `turn_done` hook runs once, sees scrubbed `KELIR_NOTIFY_EVENT`, and writes exactly one `notify` event. `tests/review_m11.rs:385-438` adds failed (`error`) and paused (`paused`) dispatch. Unit tests `src/notify.rs:426-489` prove exact OSC bytes, control-byte sanitisation, per-dir rate limit, per-kind config and rate-limit outcome. Live drive: hook marker fired, log had `"type":"notify","event":"turn_done","outcome":"ok","hooks":1`. No exit-code changes were observed.
- **Names / child sessions / Unix:** name resolution is exercised by M9 tests; Linux gate and Linux footprint cover Unix. The full Linux remote half passed after r5 was ignored.
- **Docs drift:** `cargo test --lib docs` passed in both gate halves; `docs/reference/attach.md` and `docs/reference/notify.md` exist and are generated consistently.

## Gate (exact counts)

Full gate at `efe00dc` on Windows with `bash scripts/gate.sh --linux=the maintainer`: fmt ok, clippy ok, nextest **1430 run / 1429 passed / 1 failed / 3 skipped** (retry: `kelir::review_m5a the_skills_verb_can_restore_an_earlier_body`, a known STEER-7 flake — not M11), doctest ok; Linux half failed from the same deterministic r5; **gate FAIL**.

After marking only the deterministic failing proof `#[ignore]` (`e516012`): Windows `cargo nextest run --no-fail-fast --retries 2 -j 4` — **1429 passed / 4 skipped**; Linux `bash scripts/remote-check.sh the maintainer kelir-verify-m11rev` rc=0 with fmt, clippy -D warnings, doctest and full nextest **1430 passed / 4 skipped**. Attempts to rerun the entire combined gate in fresh Herdr panes failed on pane/WSL path quirks before the gate stages ran; the two halves were run separately as above. No four-STEER-7-memory-scan failure pattern appeared; only the single M5A flake needed a retry.

## Footprint (both OSes)

- Windows: `bash scripts/live/m11rev-footprint.sh` on a 16 MB / 100k-event log measured **9,138,176 B (8.71 MiB)** and **8,970,240 B (8.55 MiB)** — under 10 MB.
- Linux: `scripts/live/m11rev-footprint-linux.sh` (copied through `hc file write --on the maintainer`, run with `hc exec --on the maintainer --timeout 600`) built the release binary from the committed HEAD, appended events while sampling `/proc/<pid>/status`: peak **8,332 kB (~8.14 MiB)** — under 10 MB. Earlier static-log samples: 8,340 kB. Kill was by exact PID via a trap; no image-name kills.

## Live drive (real Herdr panes, real model)

Release binary 12 MB. Model `deepseek/deepseek-v4.1-flash-1017` via the configured provider. Pane `m11rev-attach1` ran `kelir attach` on a session created by `m11rev-session-pane.sh`: replay rows appeared and new events streamed live. Non-JSON pane `m11rev-live2` with a hook on `turn_done`: `hc wait` returned `reason:"status", status:"done"`, `hc get` returned `{'state':'done'}`, the hook marker was written, and `events.jsonl` contained one `notify` event with `event:"turn_done", outcome:"ok", hooks:1`. A `--json` pane was also opened; as documented in `docs/DEVIATIONS.md:608-611`, `hc wait` cannot derive status from it (S3).

## Known gaps verdict

- **Per-job notify is still the M9 run-end list** (`docs/DEVIATIONS.md:1815-1818`): acceptable for M11 as a documented migration gap, but the reference doc must be corrected (S1).
- **No inline steer viewport; `--steer` reads stdin** (`docs/reference/attach.md:29-31`): acceptable — the brief only requires token-gated say/stop and read-only-by-default, which the implementation satisfies; the `--follow` coupling should be documented (S2).

## Process hygiene

Real-process reviewer tests use `TreeGuard`, `MAX_LIVE=6`, a `Slot` Drop guard and a watchdog (`tests/review_m11.rs:1-64`). Only exact PIDs were killed; no group-0 signals, no image-name kills, no visible windows from tests. `tasklist /FI "PID eq 72024"` shows the earlier leftover process is gone. Scratch scripts under `scripts/live/` contain no keys or private paths; keys come from the environment.

## Fix status (M11fix, job/m11fix)

_The findings above are the original review and are left intact. This section records the fixes_
_verified by the follow-up session._

- **B1 (blocker) — fixed.** `src/session.rs` `note_bg` now calls `self.notify("bg_settled", ...)`
  immediately after recording `BgSettled`, through the same `Session::notify`/`notify::fire` path as
  `turn_done`/`error`/`paused`. The existing `BgSettled` dedupe means exactly one notify per settled
  job, even with duplicate reports, and a turn ending alongside it fires its own kind separately.
  The reviewer's proof `tests/review_m11.rs::r5_a_settled_bg_job_fires_its_notify_hook` is un-ignored
  and now asserts one `notify` event of kind `bg_settled` plus the hook marker; a new regression
  `r9_a_settled_job_and_its_turn_notify_once_each` checks no double dispatch (one `bg_settled` and
  one `turn_done`).
- **S1 — fixed.** `docs/reference/notify.md` now states plainly that `[notify]` in `config.toml`
  owns the five per-event hooks and that a `jobs.toml` per-job `notify` is the older list that runs
  once at job run end; the DEVIATIONS appendix was aligned with it.
- **S2 — fixed.** `kelir attach --steer` without `--follow` now exits 2 with
  `--steer requires --follow` instead of silently ignoring the flag (`src/attach.rs`); documented in
  `docs/reference/attach.md` and covered by `r8_steer_without_follow_is_refused`.
- **S3 — left as documented.** The Herdr/--json status limitation stays (DEVIATIONS ~609); no code
  change, as decided.
- **S4 — kept and cleaned.** The Windows and Linux footprint probes remain under `scripts/live/`
  with paths parameterised by env vars; no personal paths or keys. Exact-PID kill via trap only.

Gate and counts: `bash scripts/gate.sh --linux=the maintainer` on committed HEAD `5d0b2ff` (logs in
`C:/Users/the maintainer/kelir-scratch/gate-job-m11fix-20261121T053227Z/`): fmt ok, clippy `-D warnings` ok,
Windows nextest **1432 run / 1432 passed / 3 skipped, 0 retried**, doctest ok; Linux (the maintainer) fmt,
clippy, full nextest **1433 run / 1433 passed / 3 skipped**, doctest ok, `remote-check rc=0`;
**gate: PASS**. No STEER-7 memory-scan or M5A flake appeared in this run. An earlier gate at
`6442557` failed on Linux only: the two new bg proofs used `sleep 1`, which completed before
`promote_after = 1` could fire on that host; lengthened to `sleep 5` in `5d0b2ff` (default shell,
no `bash` assumption).
Live-drive verdict: reviewer sign-off on the fix still pending (the original M11 live drive
remains valid; B1's change is an additional dispatch on the same notify path).
