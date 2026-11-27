# kelir Constitution

Reverse-engineered from `AGENTS.md`, `docs/SPEC.md` and the shipped code at `main` e1cd8ea. It records rules the repo already enforces. Ratified on first adoption of spec-kit.

## Core Principles

### I. Small footprint is a feature
Binary <= 20 MB, idle RSS <= 30 MB, no SQLite, no daemon. Startup and idle cost outrank features. A dependency is added only when the spec names it or the commit justifies it.

### II. Durable by construction
One process and one append-only JSONL event log per session. A crash, restart or dropped connection must not lose work in flight; resume replays the log.

### III. Freedom to the model
No approval prompts, no prompt policing, no sandbox around the shell. Opinions live in skills and gates (user-configurable), never in the core loop.

### IV. Core knows no feature
A small `core/` loop with `Feature` impls plugged into its boundaries. `Session` has at most 15 fields; features know the boundary trait, not each other. Production code stops at the first `#[cfg(test)]` and is at most 800 lines per file (exceptions in `scripts/shape-allow.txt`). Enforced by `scripts/shape.sh`.

### V. Scriptable and observable
Every capability is reachable headlessly (CLI, NDJSON output, ctl/attach/tail) as well as from the TUI. Native tool calls carry a required short `description`. Requests are cache-stable by construction.

### VI. Three operating systems, first class
Linux, macOS and Windows with CI on all three. No hardcoded `/` paths, `/tmp` or Unix-only APIs without a `cfg` and a Windows branch. One child-process runner (`src/sys/runner.rs`).

### VII. Test pyramid (NON-NEGOTIABLE)
Unit tests, e2e against a scripted mock OpenAI-compatible server, and a live drive of the release binary. Tests are filed by feature under `tests/it/`. `scripts/gate.sh` (shape, fmt, clippy, nextest) must pass before merge.

## Constraints

- Public repo: no personal data, hostnames, usernames, keys or private paths in any file, fixture or log. Keys come from the environment or a `key_cmd`.
- Comments say what and why, never when; history lives in git, `docs/PROGRESS.md` and `docs/DEVIATIONS.md`.

## Workflow

- `docs/SPEC.md` is authoritative; a deviation is recorded with its reason in `docs/DEVIATIONS.md`.
- Small commits on `main`, never a force-push or history rewrite.
- Specs under `specs/` describe shipped behaviour and must be updated in the same change that alters it.

**Version**: 1.0.0 | **Ratified**: 2026-11-27 | **Last Amended**: 2026-11-27
