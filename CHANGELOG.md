# Changelog

All notable changes to kelir are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
semantic versioning. The milestone-by-milestone evidence (test counts, commit
ids, reviewer verdicts) is in `docs/PROGRESS.md`.

## [Unreleased]

### Breaking

- The event log is format 1. `session_started` carries `format: 1` and a reader
  refuses a log written in another format ("written by a different kelir format;
  start a new session"); there is no compatibility reader. The 16-type vocabulary
  (down from 24): `usage` folds into `assistant.usage` (with `step_usage` for
  requests that have no assistant event, such as compaction and gate scoring);
  `tool_started`, `tool_settled`, `mcp_started`, `mcp_settled`, `bg_started`,
  `bg_settled`, `child_started` and `child_settled` become `call_started` and
  `call_settled` with `kind: tool|mcp|bg|child` and kind-specific fields under
  `info`; `warning` and `notify` become `note` with `note.kind` (`error` stays its own type); the
  write-only `resumed` event is gone. Consumers of `events.jsonl` must update.

- `kelir docs --find` is now `kelir docs --search`; it prints `topic:line: text` hits,
  best-matching topic first. `kelir docs --generate` and `kelir docs --check` are removed:
  `docs/reference/` is hand-maintained markdown and `cargo test --lib docs` is the one
  drift check (code names must appear in the reference). `KELIR_DOCS_ROOT` is no longer read.

- `[hint]` is gone; the pushed hint is now the built-in `hint` gate declared with
  `[gates.hint] builtin = "hint"` and a `model`. A config that still has `[hint]`
  gets the ordinary unread-key treatment.
- Gates may answer with `context` (and `kind`): built-in and external gates
  share the append path into a `source` event at the tail. A built-in's own
  provider spend is a `usage` event named after the gate (`kind: "hint"`).
  A failed built-in's note is an `error` event with `where: "gate"` (the old
  core hint used `where: "source"`).

### Added (milestones M0 – M11, in landed order)

- **M0** — the harness skeleton: the event log (`events.jsonl`), the `bash`
  tool with bounded output and spill to `blobs/`, the fold/read-back path, the
  first provider integration and the review harness.
- **M1** — process handling: Job Object assignment before spawn on Windows,
  tree kill on failure, and the no-visible-window guarantee.
- **M2** — model defaults and provider model metadata, model lists and retries.
- **M3 / M4** — IPC and session locking, named jobs and `bg` background calls,
  the concurrency governor, crash-injection tests and the shared gate lock.
- **M5-A / M5-B** — sources and skills (the harness reads its own `AGENTS.md`
  and skill directories), the `source` event, the `skills` verb; then
  improvements to shell command shaping and short-vs-long command parity.
- **M6** — TUI v2: a redrawn interactive session with facts, scrollback and
  countdown displays for retries and waits.
- **M7** — review channels, prompt-submit gates and the bridge protocol for
  driving sessions from another process.
- **M8** — MCP support: `mcp.toml`, `kelir mcp`, the session broker and channel
  servers, with an allow-list trust model.
- **M9** — two providers with credential-store reads, key-source logging,
  expiry handling and a per-provider concurrency cap.
- **M10** — TUI v3 and attach-viewer work, plus MCP and gate hardening.
- **M11** — attachments and notifications: file/attachment handling and
  notify hooks under the scrubbed environment.
- **STEER-9** — de-flaked the e2e suite; three consecutive loaded gates with
  zero retries.

### Breaking

- Cost tracking is gone: the `[prices]` table, `warn_cost`, the `cost` and
  `cost_source` fields of `usage` events, the cost column of `kelir ls` and
  `sessions ls`, cost in `status` and job STATUS, and the TUI's cost state.
  Token counts, `warn_prompt_tokens` and the cache-hit ratio stay. A config
  with `prices` or `warn_cost` gets the ordinary unread-key treatment (no
  special "removed" message); old logs are not read by the rewritten binary.

### Rewrite (issue #16)

- Cut `jobs.toml` and `kelir job run|status|history`; scheduled runs are
  `kelir run --name --single-flight` with `--timeout`, `--task-file` and
  `--fresh`. STATUS is one `names/<name>.status.json` per name, no history.
- Removed module-static run state: the deadline watcher and finalize path
  share a per-run `Arc<RunState>`.

### Changed

- The gate now caps itself at four build jobs and four test threads, takes a
  single machine-wide lock, and names any test that needed a retry.
- Crash-injection runs disable core dumps on every gate path.
- Repo hygiene: the published tree contains no private names, hosts or paths.

### Security

- Session directories and tokens are owner-only; key values are never logged,
  only their source; child environments are scrubbed before spawn.
- Signed bg status, per-job capabilities, per-child links and permission
  read-back are gone. A session token, owned-job checks and settle-once keep
  other users and duplicate reports out. `bg_started` no longer carries
  `pubkey`; status files are plain JSON.

## [0.1.0] — pending release

The first published version. It corresponds to the state of `main` at the
publication commit recorded in `docs/PUBLISH.md`. The changelog above is the
release note; the detailed milestone table is in `docs/PROGRESS.md`.
