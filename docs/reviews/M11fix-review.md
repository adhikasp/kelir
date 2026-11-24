# M11fix review — fresh QA on the fix pass

**Scope:** `C:/Users/the maintainer/kelir-wt/m11fixrev`, branch `job/m11fixrev` (based on `job/m11fix` at
`faa3049`). Diff under review: `82eeb16..faa3049`, plus reviewer commit `1d3c163` (test-only fix
for a flaky proof; no `src/` edits by the reviewer). Machine name `the maintainer` appeared only on the
gate command line.

**Verdict: PASS — 0 blockers, 1 nit.** The original M11 blocker B1 is genuinely closed: a settled
background job dispatches exactly one `bg_settled` notify through `notify::fire` and writes exactly
one `notify` event. S1/S2 are fixed; S3 is left documented by decision; S4's probes are intact and
cleaned. Both halves of the full gate are green. The one finding below is a test-design nit in a
reviewer proof, already fixed in `1d3c163`.

## Findings

| ID | Severity | Status | File:line | Concrete scenario / evidence |
|----|----------|--------|-----------|------------------------------|
| B1 | blocker | **closed** | `src/session.rs:3777-3789` | `note_bg` dedupes on an existing `BgSettled` event, then calls `self.notify("bg_settled", &text)` on first settle. Proved by `r5` and `r9` (below). |
| S1 | should-fix | **fixed** | `docs/reference/notify.md:25-31`, `docs/DEVIATIONS.md:1815-1818` | Reference now plainly separates the five `[notify]` per-event hooks from the older `jobs.toml` per-job run-end list; the appendix is aligned. |
| S2 | should-fix | **fixed** | `src/attach.rs:329-333`, `docs/reference/attach.md:29-33` | `--steer` without `--follow` exits 2 with `--steer requires --follow`; documented and covered by `r8`. With `--follow` it still accepts only `say`/`stop` and remains read-only-by-default. |
| S3 | should-fix | **left documented (by decision)** | `docs/DEVIATIONS.md:608-611`, `src/herdr.rs:74-78` | `--json` excludes the OSC status writer so NDJSON stays parseable; `hc wait` cannot derive pane status. No code change, as decided; documented gap only. |
| S4 | should-fix | **fixed** | `scripts/live/m11rev-footprint.sh`, `scripts/live/m11rev-footprint-linux.sh` | Both probes remain in `scripts/live/`, parameterised by `KELIR_BIN`/`KELIR_FOOTPRINT_ROOT`/`M11_VERIFY_TREE`; no keys or personal paths; kill is exact-PID via an EXIT trap. |
| N1 | nit | **fixed by reviewer** | `tests/review_m11.rs:391-445` (fixed in `1d3c163`) | The shipped `r9` asserted exactly one `turn_done` notify, but the script drives **two turns** (the tool turn "carrying on" and the bg-wake turn "noted"), so two `turn_done` notifies are correct. On Linux it flaked `(bg=1, turn=2)` vs expected `(1,1)`. I proved with a temporary log-dump test that both turns complete and each writes one `turn_ended` + one `notify`; the product is correct, the assertion was racy. Rewritten to assert the real invariant: exactly one `bg_settled`, and at most one `turn_done` per completed turn. |

## Proving commands and results (all on committed HEAD)

Required proofs (before my test fix, at `faa3049`):

- `cargo test --test review_m11 -- r5_a_settled_bg_job_fires_its_notify_hook r9_a_settled_job_and_its_turn_notify_once_each --nocapture`
  → **2 passed, 0 failed, 0 ignored** (r5 un-ignored; hook marker written; log holds exactly one
  `notify` event of kind `bg_settled`).
- `cargo test --test review_m11 -- r8_steer_without_follow_is_refused --nocapture` → **1 passed**;
  exit code 2 and the clear `--steer requires --follow` message.

After the reviewer's test fix (`1d3c163`):

- `cargo test --test review_m11 r5_a_settled_bg_job_fires_its_notify_hook` → pass (5 runs on this
  host; plus once in each gate half).
- `cargo test --test review_m11 r9_a_settled_job_and_its_turn_notify_once_each` → **5/5 pass**
  locally, and pass in both gate halves at `1d3c163`.
- `cargo test --test review_m11 -- --test-threads=4` → **9 passed / 0 failed** (the suite has a
  `MAX_LIVE=6` cap and is written for nextest's one-process-per-test; plain `cargo test` with its
  default 9-way thread count trips that cap by design, not a product failure).
- `cargo test --lib docs` → **24 passed** (reference drift test included).

### Edge cases probed by direct code read

- **Duplicate settle reports:** `note_bg` checks `self.events.iter().any(|e| matches!(BgSettled{...}))`
  at `src/session.rs:3777` and returns before recording/notifying when already settled. `record()`
  pushes into `self.events` in memory (`src/session.rs:2350-2369`), so a second report in the same
  session cannot double-dispatch. A watcher/resume replay sees the logged event and is deduped.
- **`child_settled` vs `bg_settled`:** separate kinds and separate sites — `src/session/children.rs:561-574`
  fires `child_settled` only on the first `ChildSettled`; `src/session.rs:3789` fires `bg_settled` on
  `BgSettled`. Both use the same `Session::notify` → `notify::fire` path; neither substitutes for the other.
- **Job settling on a stopped/error turn:** notification dispatch is independent of the turn outcome —
  `note_bg` fires on settlement regardless of the turn state; the turn's own outcome fires its own kind
  via `finish_turn` (`src/session.rs:2567-2589`, callers at `:4694-4765`). `r6`/`r7` prove the `error`
  and `paused` kinds still fire on the same path; a cancelled/lost bg job's `wake_text` still produces
  a `bg_settled` notify (`src/bg.rs:948+`).
- **Scrubbed env / 30 s timeout on this path:** `r5`'s hook writes `$KELIR_NOTIFY_EVENT` to its marker
  and the marker appears, proving the bg notify hook runs with the scrubbed environment's two
  `KELIR_NOTIFY_*` variables. `src/notify.rs:18,22-28,31-41,113-150` fixes the 30 s bound
  (shortened in tests by `KELIR_TEST_NOTIFY_TIMEOUT_MS`), the allow-list env scrub and the whole-tree kill
  on hook exit; `src/notify.rs:291-311,334-388` gives per-dir/per-kind rate limiting.

## Gate (exact counts)

Full gate `bash scripts/gate.sh --linux=the maintainer` on committed HEAD `1d3c163`, logs in
`C:/Users/the maintainer/kelir-scratch/gate-job-m11fixrev-20261121T063828Z/`:

- `fmt`: ok; `clippy --all-targets -- -D warnings`: ok.
- Windows `cargo nextest run --no-fail-fast --retries 2 -j 4`: **1432 run / 1432 passed / 3 skipped**.
  One retry, named by the gate: `kelir::review_m10fix r15_two_new_conversations_get_distinct_sessions_and_both_answer`
  (unrelated to M11; STEER-9 flake). No leaks.
- `cargo test --doc`: ok.
- Linux (the maintainer) `remote-check.sh`: fmt, clippy `-D warnings`, full nextest **1433 run / 1433 passed /
  3 skipped**, doctest ok, `remote-check: the maintainer rc=0` (HEAD `1d3c163`). One retry there:
  `kelir::review_m4 r18_a_killed_session_takes_exec_grandchildren_with_it` (unrelated to M11).
- **`gate: PASS`**. No STEER-7 memory-scan pattern appeared; no leaked processes reported.
- An earlier gate at `faa3049` had the r9 proof flake once on Linux (`(1,2)` vs `(1,1)`); my
  log-dump diagnosis showed two legitimate turns, not a product double-dispatch. `1d3c163` makes the
  proof deterministic; the final gate above has **zero r9 retries** on either OS.

## Process hygiene

All real-process proofs use the suite's `TreeGuard`, `MAX_LIVE=6` cap, `Slot` Drop guard and
watchdog (`tests/review_m11.rs:20-64`). Only exact PIDs are killed; no group-0 signals, no image-name
kills, no visible windows from tests. Post-gate `tasklist`: **26 `sh.exe` (baseline 52), 1 `kelir.exe`
(baseline 20)** — no growth, no leftovers from these runs. Scratch scripts carry no keys or private
paths; keys come from the environment only.

## Live drive

Original M11 live-drive sign-off remains valid; B1 adds one notify dispatch on the same, already
live-verified `notify::fire` path, and the fix pass changed no live-facing surface except the
`--steer` usage guard. I did not spend a fresh live-model session on this fix pass; the real-binary
notify path and the attach guard are covered by deterministic mock-driven e2e above. Mark the Go
provider path "not live-verified" (`KELIR_GO_API_KEY` is not available here); it is covered by mock tests.

## Spec items not covered here

- `--json` Herdr status gap (S3) remains by decision; documented in DEVIATIONS.
- Footprint RSS numbers are from the original M11 review (Windows ~9.1 MB, Linux ~8.1 MB on a
  100k-event log); the probes remain in `scripts/live/` and were not re-measured on this fix pass,
  which changed no attach/footprint code except the `--steer` guard.
