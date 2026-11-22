# M10 review — channel mode

**Verdict: FAIL** (7 blockers, 4 should-fix, 2 nits)
Reviewer: fresh QA session on `job/m10rev` (branch based on `job/m10` at
`db20caa`). Diff reviewed: `ff09d25..db20caa`. No product code was modified.

## Test counts

`bash scripts/gate.sh --linux=the maintainer` on HEAD `e0542d4` (reviewer tests added):

- Windows: fmt ok, clippy `--all-targets -D warnings` ok, nextest
  **1360 run / 1350 passed / 10 failed / 3 skipped / 1 retried** (the retry is
  the reviewer's own `r9_restart_after_reply_has_no_duplicate_reply`, a passing
  test that timed out at 90 s on the loaded desktop; not a product finding),
  doctest ok.
- Linux (`the maintainer`): FMT-OK, CLIPPY-OK, nextest **1361 run / 1351 passed /
  10 failed / 3 skipped**, doctest ok. `remote-check: the maintainer rc=100`.
- Gate verdict: **FAIL**, with exactly the 10 deterministic reviewer failures
  below (identical set on both OSes).
- Release binary: **11 MB** (budget 20 MB). No leaked `sh.exe` or my own
  `kelir.exe`/`python.exe` processes after the real-process tests and live
  drives (checked by exact command line and PID, never by image name).

Log paths:
`C:/Users/the maintainer/kelir-scratch/gate-job-m10rev-20261120T233034Z/gate.log`,
`.../nextest.log`, `.../linux.log`.

## Findings

Severity key: **blocker** = a stated task/spec requirement is unmet or a data
loss / duplicate-reply path; **should-fix** = contract inconsistency or a
partial implementation; **nit** = cosmetic/typo.

### B1 — A torn UTF-8 tail silently erases the whole spool (data loss)

`src/channel.rs:330`: `std::fs::read_to_string(&path).unwrap_or_default()`.
If a crash cuts a multi-byte character, `read_to_string` fails and the code
substitutes an *empty* string: `good` is empty, `text` is not, so the "repair"
writes an empty file. `next_seq` restarts at 1 and every previous record is
gone. The milestone requires torn-tail repair with no loss.

Proof: `tests/review_m10.rs::r1_invalid_utf8_torn_tail_must_not_erase_the_spool`
→ `replay().len()` is 0, expected 1. Fails on Windows and Linux.

### B2 — A compacted done id is no longer deduped (duplicate reply after restart)

`src/channel.rs:489-492` drops the `inbound` record for every done id during
compaction, keeping only `done`. `Spool::open` (`:345-360`) rebuilds `seen`
only from surviving `inbound` records, so after reopening a compacted spool a
bridge resend of that id is treated as new: it is answered again. The task
requires "dedupe by message id" across the whole spool lifetime.

Proof: `tests/review_m10.rs::r2_a_compacted_done_id_must_still_be_deduped` →
`inbound()` returns `true` for the resend, expected `false`.

### B3 — Permission-relay capability check is bypassable (STEER-7 trust contract)

`src/channel.rs:38` lists only `claude/channel/permission(s)/approval` with
hyphens; `forbidden_capability` (`:527`) does exact/slash-prefix matching. A
configured `claude/channel/permission_relay`,
`claude/channel/permission/relay`, or different case passes both
`mcp::config::parse_layer` (`src/mcp/config.rs:141`) and `capabilities()`
(`src/channel.rs:536`) and is declared at `initialize` — a promise kelir
cannot keep (it has no approvals). The spec explicitly says *never* a
permission-relay capability, whatever the config says.

Proof: `tests/review_m10.rs::r4_permission_relay_variants_must_be_forbidden`,
`r4b_config_rejects_permission_relay`,
`r4c_capabilities_filter_forbidden_defense_in_depth` — all three fail.

### B4 — Bounded concurrency is not implemented (spec requirement)

The task requires "bounded concurrency across conversations (`max_active`,
default 4; the rest wait)". `src/channel.rs:1280-1310` runs `process(...).await`
in a *single* loop: `Dispatcher::take_next` marks entries running but there is
no spawn/join, so only one turn is ever in flight. A flood of two
conversations is serialised regardless of `max_active`.

Proof: `tests/review_m10.rs::r14_max_active_should_allow_parallel_conversations`
→ only one provider request arrives in 30 s (mock holds both), expected two
concurrent. Fails on both OSes. Note the unit test
`a_second_message_waits_and_two_conversations_run_up_to_the_cap` only tests the
`Dispatcher` state machine, not the driver, so it does not catch this.

### B5 — `/stop` is never marked `done`: duplicate "stopped" reply on restart

`src/channel.rs:818-830`: the `/stop` branch of `start_turn` returns `Ok(())`
without inserting an `Active` entry. `process()` writes the `done` spool record
only inside `if let Some(item) = active.remove(&conversation)` (`:1112`), so a
handled `/stop` leaves the message `claimed` forever. On restart the spool
replays it and sends another "stopped" reply — a duplicate reply for one
inbound message.

Proof:
`tests/review_m10.rs::r11_stop_is_done_in_the_spool_and_not_replied_twice` →
spool contains only `inbound` + `claimed`, no `done`. Live release-binary drive
confirms: reply `{"id":"live-stop","text":"stopped"}` with spool
`inbound`, `claimed` and **no** `done`.

### B6 — `reply_to` is always overwritten with the message id

`src/channel.rs:734`: `"reply_to": message.id`, ignoring the parsed
`Message::reply_to` (`:277`). The bridge's thread/reply-to field never reaches
the reply tool. The task lists `reply-to` as part of the protocol.

Proof: `tests/review_m10.rs::r3_reply_must_use_the_messages_reply_to` →
recorded `reply_to` is `"m-review-3"`, expected `"parent-9"`. Live drive:
inbound `reply_to: parent-7`, reply args `reply_to: "live-1"`.

### B7 — A stale gate deny poisons later turns in a named session

`src/channel.rs:619-628`: `outcome()` scans the *whole* log backwards for any
`gate` event with `decision == "deny"`. Named sessions are resumed across
messages, so an old denial makes every later turn whose `turn_ended` has not
yet been written be reported as `Outcome::Gated` ("blocked by policy") — the
wrong reply and the wrong fallback text.

Proof: `tests/review_m10.rs::r13_a_stale_gate_deny_must_not_poison_a_later_turn`
→ returns `Gated("old denial")`, expected `Failed`.

### S1 — `read_status()` reads a path the driver never writes

`src/channel.rs:582` reads `state/channel-status.json`; the driver writes
`state/channel-status/status.json` (`:1037`, `:1042`), which is what
`docs/reference/channels.md:53` and `docs/reference/files.md:54` document.
The public reader silently returns `None` for every real driver. No in-tree
caller exists today, so this is a latent API/doc mismatch, not a live failure.

Proof: `tests/review_m10.rs::r7_status_snapshot_path_matches_docs`.

### S2 — Dead fallback constants / mislabelled gate fallback

`PAUSED_FALLBACK`, `TIMEOUT_FALLBACK` and `GATED_FALLBACK`
(`src/channel.rs:57-61`) are never used: `process()` (`:1095-1100`) sends
`cfg.fallback` for all of `Failed` and `Gated`, and `outcome()` (`:616`)
collapses paused/timeout/error into one string. A user cannot tell a policy
block from a provider failure, and the configured fallback is the same text
for all cases. The task says "one short fallback reply" so this is not a
blocker, but the documented distinction is unimplemented.

### S3 — "logged once" is per notification, not per shape

`docs/reference/channels.md:23` says unknown/malformed notifications are
"dropped and logged once". `src/channel.rs:1237` logs one line per bad
notification with no dedupe, so a flood of malformed frames produces an
unbounded stderr stream. No payload is echoed (good, STEER-7), but the once
contract is not met.

### S4 — Five kill points are only partly verified

The implementer retro admits the e2e suite does not kill the driver at the
five stated points. Reviewer tests cover restart after `done` (`r9`, passes)
and a mid-turn kill (`r10`, passes), but there are no deterministic tests for
"after spool write", "after user event", "after final answer before reply", or
"after reply" with a controlled kill. The protocol relies on the bridge's
idempotency by `message_id` (DEVIATIONS acknowledges a bridge that ignores it
can send twice); this is a documented gap, not a hidden one.

### N1 — Brief file `docs/tasks/M10.md` absent from this branch

The reviewer prompt names `docs/tasks/M10.md`; it does not exist on
`job/m10rev` (it was added in commit `13dbdc1` on another branch). I reviewed
against `git show 13dbdc1:docs/tasks/M10.md`, which matches the task text in
the prompt. **Orchestrator action:** make sure the brief lands with the
milestone (or point the review at the canonical brief).

### N2 — `job/m10` is based on `job/m9`, not `main`

As flagged in the reviewer prompt: the final merge into `main` may conflict.
Orchestrator action: merge `job/m9` first (or resolve conflicts explicitly),
then re-run the gates on `main`.

## What passed (adversarially verified)

- Capability negotiation declares only `claude/channel` and sends it at
  `initialize` (live + e2e: `{"claude/channel":{}}`); e2e asserts no
  permission capability. (Except the B3 variants.)
- Spool fsync before processing, `inbound`/`claimed`/`done` append-only, and
  replay-after-`done` with no duplicate reply (`r9`, plus e2e `dedupe_replayed_id`).
- Envelope: `user` event with `from: channel`, remote-user/untrusted warning,
  `reply-to` and attachment pointers (`tests/e2e_m10.rs`, live log).
- Provider failure → exactly one fallback reply (`tests/e2e_m10.rs::one_fallback_on_failure`).
- No canary from `config.toml` env in events, spool, stdout or stderr
  (`r8`, e2e, live grep: `none`).
- `turn_done` notification sent on turn end (e2e + live).
- OSC 7501 status: `Working` while a turn runs, `Idle` otherwise
  (`src/channel.rs:1084`, `:1126`).
- Docs drift test passes (gate `cargo test --lib docs` included in nextest).
- No leaked processes: `sh.exe` count stayed at 1 throughout; no leftover
  processes matched my scratch paths after any test or live drive.

## Live drive (Herdr panes, release binary, fake bridge + mock provider)

Command (script committed as `scripts/live/m10_review_channel.sh`):

```
hc run --bg --name review-m10-live3 --cwd C:/Users/the maintainer/kelir-scratch \
  'C:/Program Files/Git/bin/bash.exe' -c \
  'cd C:/Users/the maintainer/kelir-wt/m10rev && bash scripts/live/m10_review_channel.sh'
hc wait review-m10-live3; hc read review-m10-live3
```

Scenario 1 — one inbound DM answered:

- status line: `{"conversations":0,"queue_depth":0,"active":0,"spool_backlog":{},"replies":0,"fallbacks":0,"runtime":"channel"}`
- replies: `{"id":"live-1","text":"LIVE answer from mock provider","duplicate":false}`
- reply args: `{"conversation_id":"live-chat","message_id":"live-1","reply_to":"live-1","text":"LIVE answer from mock provider"}` ← B6
- bridge events: `inbound`, `initialize` with `{"claude/channel":{}}`, `turn_done`
- spool: `inbound`, `claimed`, `done`
- user event: `from:"channel"` with the untrusted-data envelope
- canary grep: `none`
- **Verdict: PASS for answer/fallback/status/capability/envelope; B6 for `reply_to`.**

Scenario 2 — `/stop please`:

- replies: `{"id":"live-stop","text":"stopped","duplicate":false}`
- spool: `inbound`, `claimed`, **no `done`** ← B5
- no user event (correct: `/stop` is a verb, not a prompt)
- **Verdict: FAIL for spool durability (B5).**

No real Telegram credential exists in this repo; per the brief, none was
guessed or requested. The real-bridge live layer is **not verified**; the fake
bridge + mock provider is the verification vehicle. `KELIR_GO_API_KEY` is not
available: the Go path was not live-verified (unchanged from prior milestones).

## Spec items not covered by any test

- One named session per conversation resumed across restarts is exercised only
  implicitly (`r9` restart with one conversation).
- Second message during a turn is queued (`Dispatcher` unit test only, no e2e).
- Flood of 1000 messages through the real driver (unit `Dispatcher` only).
- Rate limits through the real driver (unit `RateLimits` only).
- Gate denial of an inbound prompt through the real driver (unit `outcome()`
  only; no e2e with a configured gate).
- Attachments (`data` base64, `path`) through the real driver.
- OSC 7501 state transitions observed from Herdr (`hc read` of a live pane did
  not expose the OSC sequence; code review only).

## Files committed on this branch

- `tests/review_m10.rs` — 15 red/passing adversarial tests.
- `scripts/live/review_channel_server.py` — fake bridge that records reply args.
- `scripts/live/m10_review_channel.sh` — Herdr live-drive script.

All commits are `M10-review: …` with explicit paths; no product code touched.

## Fix status (m10fix)

All 7 blockers and 4 should-fix addressed on `job/m10fix`; the 10 failing reviewer
tests in `tests/review_m10.rs` now pass unchanged.

- B1 fixed: spool opens as bytes, torn UTF-8 tail cut and kept in `.torn`.
- B2 fixed: a `done` record also marks the id seen, so a compacted id is still deduped.
- B3 fixed: `forbidden_capability` refuses any name containing `permission`/`approval`.
- B4 fixed: turns run concurrently up to `max_active` (LocalSet on a large-stack thread).
  This also fixed a latent bug the review did not name: every `run --name` installed the
  process-wide plan, so a second NEW conversation in one process failed.
- B5 fixed: `done` is written for every processed message, including `/stop`.
- B6 fixed: reply uses the message's `reply_to`, else its id.
- B7 fixed: a gate deny followed by a later `user` event is not the outcome of that turn.
- S1 fixed: `read_status` reads `channel-status/status.json`.
- S2 fixed: default fallback text is chosen per cause; a configured text wins.
- S3 fixed: a malformed shape is logged once.
- S4 partly: spool-state kill tests added (after write / mid-turn / after done); an
  automated kill -9 of the driver at each of the five points is still open (DEVIATIONS).
- N1/N2: orchestrator items, unchanged.
- Extra (m10fix): the driver reconnects a channel server that failed to start or died
  (checked every 500 ms; a failure is logged once). Under parallel load the fake bridge
  sometimes failed its first start ("cannot assign it to the job") and the driver never retried.
- Extra (m10fix): `tests/e2e_m9.rs::the_next_day_adds_the_date_source_and_nothing_else` used
  the real calendar date as "tomorrow" and began failing on 2026-11-21; its dates are now fixed
  far-future values (unrelated to M10, needed for a green gate).
