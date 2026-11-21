# M10 fix-pass review — channel mode

**Verdict: PASS** (0 blockers, 0 should-fix, 1 residual gap recorded as non-blocking)

Reviewer: fresh QA kelir session on `job/m10fixrev` (based on `job/m10fix` at
`590b019`). Diff reviewed: `930589c..96594d5` (fix commits `28bdf17`, `b4e4406`,
`188d3a9`, `590b019`, plus two reviewer-only test commits `396f788`, `96594d5`).
No product code in `src/` was modified. I did not trust the fixer's summary: every
blocker was re-proved with a test or a direct code read.

## Test counts (exact, final HEAD `96594d5`)

`bash scripts/gate.sh --linux=the maintainer`, logs in
`C:/Users/the maintainer/kelir-scratch/gate-job-m10fixrev-20261121T014431Z/`:

- Windows: `fmt` ok, `clippy --all-targets -D warnings` ok,
  `nextest` **1387 run / 1387 passed / 3 skipped / 0 retries**, doctest ok.
- Linux (`the maintainer`): FMT-OK, CLIPPY-OK, nextest **1388 run / 1388 passed /
  3 skipped**, NEXTEST-OK, DOCTEST-OK, `remote-check: the maintainer rc=0` (HEAD
  `96594d5`, work dir `~/kelir-verify-m10fixrev/src-tree` kept).
- Gate verdict: **PASS** (`gate.log`).
- Reviewer suite `tests/review_m10.rs`: **14/14 PASS** in the gate. The shipped
  suites are all green; the 3 skips are the same platform-gated ones as before.
- My added proving tests `tests/review_m10fix.rs` (4 tests): all PASS in the
  gate (r15, r17, r18, r20). Repeated locally 6/6 green.
- Release binary: **11,648,512 bytes (~11.1 MB)**, budget 20 MB. `cargo build
  --release` rc=0.
- No leaked processes: no `kelir.exe`/fake-bridge `python.exe` from my runs;
  `sh.exe` stayed at the 2-process baseline. No visible windows (all driver
  children go through the harness `TreeGuard`, `CREATE_NO_WINDOW`/Job Object).
- No secrets in the diff, logs or test fixtures; canary probes pass (r8, r20,
  e2e). No new dependencies. Docs drift test included in nextest (24 `docs::`
  tests), green.

## Proving commands

```
cargo test --test review_m10                 # 14/14 (repeated: 3/3 full, 4/4 serial)
cargo test --test review_m10fix              # 4/4 (repeated 6/6)
cargo test --test review_m10 r14 ...         # B4, repeated 6/6 isolated
bash scripts/gate.sh --linux=the maintainer            # PASS; exact counts above
cargo build --release && stat -c%s target/release/kelir.exe   # 11648512
tasklist /FI "IMAGENAME eq kelir.exe" ...     # no leaks
```

## Blocker status — all closed

| ID | Status | Evidence (file:line) | Proving test |
|---|---|---|---|
| B1 torn UTF-8 erases spool | **closed** | `src/channel.rs:329` reads bytes; `:331-364` keeps only fully-parsed good records and writes the exact bad suffix to `.torn` at `:366-367`; `done` ids now `seen` at `:360`. | `review_m10.rs::r1` + my **r17** (multi-byte cut: both good records survive byte-for-byte, `.torn` holds exactly the torn bytes) |
| B2 compacted done id deduped | **closed** | `src/channel.rs:358-360` inserts every `done` id into `seen` on open/replay. | `review_m10.rs::r2` (reopen + resend → `inbound()` false) |
| B3 capability filter | **closed** | `src/channel.rs:528-534` `forbidden_capability` matches `permission`/`approval` substrings, any case/separator; `mcp/config.rs:140-146` rejects at parse; `channel.rs:547-550` filters at `capabilities()`, wired to `initialize` at `mcp/broker.rs:277` → `mcp/rpc.rs:311-314`. | `review_m10.rs::r4/r4b/r4c` + my **r20** (4 variants refused at config; env secret never in error) |
| B4 bounded concurrency | **closed** | `src/channel.rs:1382-1390` `spawn_local` per message up to `max_active` (dispatcher `:981-996`); one `LocalSet` on a 256 MB-stack thread (`:1200-1235`), no `Send` requirement, no borrow across `.await`. Latent fix: `jobs::with_next_id` task-local (`src/jobs.rs:88-108`) replaces the process-wide `NEXT_ID`/plan, `channel.rs:860-867`. | `review_m10.rs::r14` (peak concurrency 2) + my **r15** (two NEW conversations → two distinct named session ids, both answer, no canary) |
| B5 `/stop` done + one reply | **closed** | `src/channel.rs:829-834` `/stop` returns `Ok(None)` → `Outcome::Done("stopped")` at `:1132`; `done` written for every message at `:1149-1151`. | `review_m10.rs::r11` + my **r18** (one "stopped" reply, spool has `done`, restart sends no second reply) |
| B6 reply_to from inbound | **closed** | `src/channel.rs:745` `"reply_to": message.reply_to.as_deref().unwrap_or(&message.id)`. | `review_m10.rs::r3` (bridge records `reply_to: parent-9`) |
| B7 stale gate deny | **closed** | `src/channel.rs:623-636` `outcome()` scans backwards, stops at the newest `User` event; a deny before it no longer poisons a turn with no `turn_ended`. | `review_m10.rs::r13` (old deny + later user → `Failed`, not `Gated`) |

## Should-fix status — all closed

- **S1 status snapshot path** — **closed**. `read_status`
  (`src/channel.rs:587`) and `write_status` (`:1034-1039`) both use
  `channel-status/status.json`; `review_m10.rs::r7` passes. There is still no
  live `ctl status` caller of the channel reader (the original finding was a
  latent API/doc mismatch, not a live failure); paths and docs now agree.
- **S2 per-cause fallback text** — **closed with a deliberate choice**.
  `fallback_text` (`src/channel.rs:1086-1099`) returns `GATED_FALLBACK` /
  `TIMEOUT_FALLBACK` for those causes and a configured text wins for all;
  `b4e4406` removed the special "session paused" text because a provider
  failure ends the turn `paused` (`src/session.rs:4708-4714`), so calling it a
  pause would mislabel a provider error. The default fallback covers it; the
  task requires only "one short fallback reply". Unit test
  `fallback_text_names_the_cause_unless_configured` (`:1662`) passes.
- **S3 log-once per shape** — **closed**. `src/channel.rs:1326-1331` dedupes by
  shape (`logged_drops.insert(format!("{reason:?}"))`) before logging, still
  never echoing the payload. Proved once end-to-end with a temporary 12-message
  flood test (exactly one `dropped a malformed` line, no canary in stderr);
  that test was removed from the shipped file because the fake bridge's
  poll-on-stdin design makes it flaky under parallel load. The shipped
  `review_m10.rs::r8` (no canary in drop reason) and unit tests remain green.
- **S4 five-point kill -9** — **PARTLY fixed, honestly documented**. Spool-state
  kill tests exist (inbound/claimed/done survive restart:
  `src/channel.rs:1680-1697`, `review_m10.rs::r9`, my r18), plus the reviewer's
  restart tests. An automated controlled kill of the driver at all five points
  is still open and stated verbatim in `docs/DEVIATIONS.md:1779-1782`. Per the
  brief this is a **residual gap, not a blocker**.
- **N1 brief file** — **resolved**: `docs/tasks/M10.md` is present on this branch.
- **N2 base branch** — orchestrator item (merge `job/m10fix` into `main` by
  the merge owner); not a code finding.

## Extra fixes verified, no regression

- Reconnect of a channel server that failed to start or died
  (`src/channel.rs:1263-1265`, `:1301-1317`): tested via the r15/r18 drivers;
  the job-object start race under load is a harness artifact, and the reconnect
  is logged once until recovery. `from: channel` for named channel sessions
  now comes from `jobs::channel_mode()` (`src/session/children.rs:1083`).
- The m9 date test calendar fix (`tests/e2e_m9.rs`) is green in both gates.
- I observed `review_m10.rs::r14` fail once in ~15 cold parallel *local* runs of
  that single file (never in isolation — 6/6 — never serial — 4/4 — and never
  in either gated nextest run). It is a load/timing flake of the two-process fake
  bridge, not a product serialization failure: my **r15** and a temporary held-
  mock diagnostic both proved real two-conversation parallelism and distinct
  session ids. Not a finding, recorded for the STEER-9 flake tracker.

## Live drive

No real bridge credential exists in this repo; per the brief, none was guessed
or requested. The verification vehicle is the fake bridge + scripted mock,
driven by the real release/debug `kelir channel` binary through the reviewer and
e2e suites (r3, r9, r14, r15, r18 and `tests/e2e_m10.rs`). The Go path
(`KELIR_GO_API_KEY`) is not available and remains unverified live (unchanged).
The original M10 review's real-model live log stands; this fix pass changed no
provider-facing behaviour, only channel driver logic.

## Files committed on this branch (reviewer only)

- `tests/review_m10fix.rs` — 4 proving tests (r15, r17, r18, r20).
- `docs/reviews/M10fix-review.md` — this report.
- `C:/Users/the maintainer/kelir-scratch/m10fixrev.done` — verdict file (outside repo).
