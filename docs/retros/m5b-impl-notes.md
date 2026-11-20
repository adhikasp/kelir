# M5-B implementation notes — Herdr line, hint, memory, media, observe

A previous session on this job was cut off mid-work (leaf modules `herdr_ctx`, `hint`, `memory`
and config were written; `observe`, the session wiring, media and docs were not, and
`src/sources.rs` had one byte-corrupted dash that stopped the crate compiling). This session
kept the sound leaves and finished the rest.

## What was built

- **Herdr line** (`src/herdr_ctx.rs`, wired in `prepare`): `hc get $HC_TERMINAL_ID --json` once,
  <= 1 s, through `credentials::run_command` (PATH lookup, `.cmd` shim, no console window, tree
  killed past the deadline). One line at the end of `# Environment`; a `sources[]` entry of kind
  `herdr` with no path. Failure => an `error` event (`where: source`) and no line. The probe runs
  on `spawn_blocking`, so the runtime is never held for the second.
- **Pushed hint** (`src/hint.rs`, `Session::maybe_hint`): off unless `[hint] model` is set. At
  `before_step`, the newest human (`from != job`) user event not yet scored (`hint_seen` seq; a
  resume starts past the log's own prompts) gets one non-streaming scoring call under
  `timeout_ms`. The answer is one `source` event, kind `hint`, at the tail. The scorer is built
  on first use; a provider endpoint without a key, a timeout, non-2xx, garbage or invented names
  all fail open with at most one `error` note.
- **Memory** (`src/memory.rs`): `memory_dir` notes become `memory` sources (a `# Memory` section
  in the frozen system, `sources[]`), watched like instruction files; a note *created* later is
  found by `memory::unseen` in `check_sources`.
- **Media**: `kelir view` of an audio/video file copies it into `attachments/` and records a
  `source` of kind `attachment-note`. Images are untouched. `Watch::rebuild` skips `hint` and
  `attachment-note` events (nothing on disk backs them).
- **observe** (`src/observe.rs`, `kelir observe`, `kelir ctl <s> observe [raw]`): read-only, from
  the log, through the same `fold` and `provider::build_request` plus the same two capability
  decisions as the loop. `--raw` prints the body with no newline.

## Deviations / limits (also in `docs/reference/sources-and-skills.md`)

- `observe` is a log read, not a socket verb: it needs no session token, writes no `verb` event,
  works on a dead session, and cannot disturb a turn. `ctl observe` is routed to the same code
  client-side. A field a *live* process dropped after a 400 (`refused_fields`) is process memory
  and not shown.
- Headline test sample: a request held in flight is unchanged until its answer, so observe at
  that moment equals that request's body byte for byte (`raw_body` was added to the mock's
  `Recorded`); at rest the observed messages are a strict prefix of the next request.
- The "no key => off" path is only unit/live covered: an e2e cannot hide a real opencode store on
  the dev box, so the e2e covers the unreachable-endpoint path instead.
- Transcription/frames for audio/video are a documented later hook (same `source` shape).

## Tests

Unit: 586 (`--lib`); new e2e `tests/e2e_m5b.rs`: 15 (fake `hc` printing / failing / malformed /
hanging / absent, determinism, outside Herdr, hint off / one call / prefix stability across a
hint / four fail-open scorers / unreachable endpoint, memory edit + new note, media note,
observe --raw byte equality + prefix + human view + ctl + unknown session).
