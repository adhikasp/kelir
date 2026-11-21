# M10 implementer notes

## What landed

- `src/channel.rs`: notification parsing (bridge and generic shapes, bounded text,
  attachments), fsync'd append-only `Spool` with dedupe, torn-tail repair, claim/done
  replay and size-bounded compaction; capability declaration with forbidden
  permission relay; dispatcher (single-flight, bounded `max_active`), rate limits,
  fallback selection, envelope rendering, attachment saving, reply/turn_done,
  status snapshot and the headless `kelir channel` driver.
- `src/mcp/{config,rpc,broker,cli}.rs`: `reply_tool`, `declare_capabilities`,
  capabilities at `initialize`, a direct channel sink on the broker (the session
  loop's `mcp-notify` path is unchanged).
- Named sessions per conversation through M9 names. The task's `user` event is
  `from: channel`, gated by M8 `prompt_submit`; `/stop` sends the `stop` verb.
- Reference: `docs/reference/channels.md`, config keys, CLI, files, SPEC section,
  DEVIATIONS; generated blocks refreshed; drift test passes.
- Pyramid: 733 lib tests, one e2e test file with three sequential scenarios
  (answer+done spool+capabilities+no canary, one fallback on failure, duplicate id
  dedupe) against the fake bridge and mock provider.

## Verification

`cargo test --lib`, `cargo test --test e2e_m10`, `cargo fmt`, `cargo clippy
--all-targets -- -D warnings` all pass; the full gate is recorded in PROGRESS.

## Known gaps / hand-offs

- **Maintainer credentials/infra:** no real Telegram token or bridge exists in the
  repo; a real live drive through a bridge cannot be done here. The fake server in
  `tests/support/channel_server.py` is the verification vehicle.
- **Kill -9 at all five crash points** is covered at the spool level by unit tests
  (replay/torn tail/dedupe) and the reply id is an idempotency key, but the e2e
  suite does not kill the driver at each point; that is a reviewer/live-drive item.
- Reply idempotency is a hint to the bridge; a bridge that ignores `message_id`
  may send a duplicate if it crashes between the reply and the `done` record.
- The three e2e scenarios run sequentially in one test: three concurrent
  real-process drivers contended for the desktop's spawn budget and all timed out.
  The harness uses the shared TreeGuard and a shell-count watchdog (no image kills).
