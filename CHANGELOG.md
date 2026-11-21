# Changelog

All notable changes to kelir are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
semantic versioning. The milestone-by-milestone evidence (test counts, commit
ids, reviewer verdicts) is in `docs/PROGRESS.md`.

## [Unreleased]

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

### Changed

- The gate now caps itself at four build jobs and four test threads, takes a
  single machine-wide lock, and names any test that needed a retry.
- Crash-injection runs disable core dumps on every gate path.
- Repo hygiene: the published tree contains no private names, hosts or paths.

### Security

- Session directories and tokens are owner-only; key values are never logged,
  only their source; child environments are scrubbed before spawn.

## [0.1.0] — pending release

The first published version. It corresponds to the state of `main` at the
publication commit recorded in `docs/PUBLISH.md`. The changelog above is the
release note; the detailed milestone table is in `docs/PROGRESS.md`.
