# STEER-4 — no step limit by default

Maintainer decision (2026-11-19). Small job; run right after M5-A is reviewed and before STEER-1-fixes.

## Build
- `src/config.rs` (~line 233): `max_steps` unset or `0` means unlimited (store as `Option<u32>` / `u32::MAX`
  semantics, your call); an explicit positive value is a cap per turn. `--max-steps N` flag overrides.
- `src/session.rs`: the `turn_steps > max_steps` error path applies only to an explicit cap. When hit, the turn
  is **paused**, not failed: `turn_ended` outcome `paused` (add to the event vocabulary and docs), the visible
  message reads "turn paused after N steps; say continue to resume", exit/NDJSON `result` outcome `paused`
  (exit code 0 for `run`, documented), the session in `--keep` stays alive and the next `say` starts a new
  turn with a fresh count. Resume (M3) treats a paused turn like a stopped one.
- The TUI banner shows steps used/limit only when a limit is set.
- Docs: SPEC.md (already updated), DEVIATIONS.md, README/usage.
- After it lands the orchestrator removes `max_steps = 400` from the user config.

## Tests
Unit: config parsing (unset, 0, N, invalid). E2E (mock): 150-step scripted turn completes with no limit;
an explicit cap of 5 pauses with outcome `paused`, `--keep` session continues on `say` with a fresh count;
resume after a paused turn. Gates: fmt, clippy -D warnings, full cargo test, remote-check; no leaked processes.
