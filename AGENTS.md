# AGENTS.md — building kelir

kelir is a Rust agent harness. The spec is `docs/SPEC.md`: read it fully before writing code. It is
authoritative; if you must deviate, write the deviation and the reason in `docs/DEVIATIONS.md`.

## Roles (this repo is built by three agents)

- **Orchestrator** (Claude Code, the pane that launched you): assigns one milestone at a time, runs
  the gates, relays findings. Reports to you arrive as `<hc from="...">` messages.
- **Implementer** (opencode): writes code and tests. Never starts other agents.
- **Reviewer/tester** (Claude Code, separate pane): reads the diff, runs the unit/e2e suites, and
  drives the real binary in a Herdr pane against a real model. Writes findings to
  `docs/reviews/Mn-review.md`.

## Rules for everyone

- Work on `main` in small commits: `Mn: <what>`. Never force-push, never rewrite history.
- Public repo: no personal data, hostnames, usernames, API keys, or private paths in any file,
  test fixture or log. Keys come from the environment (`OPENROUTER_API_KEY`, `KELIR_GO_API_KEY`).
- Footprint is a feature: binary <= 20 MB, idle RSS <= 30 MB, no SQLite, no daemon. Add a
  dependency only if the spec names it or you can justify it in the commit message.
- Windows is first class. Never hardcode `/` paths, `/tmp`, `bash` on PATH, or Unix-only APIs without
  a `cfg` and a Windows branch. The dev box is Windows 11 with Git Bash; the shell for `bash` tool
  calls is Git for Windows `bash.exe`.
- Report with `hc send $HC_TERMINAL_ID "<short message + file path>"` when done or stuck. Long
  output goes in a file; send the path.

## The test pyramid (a milestone is not done until all three layers pass)

1. **Unit** (`cargo test --lib`): pure logic: fold, event serialisation, spill, edit semantics,
   SSE parsing, cache-key construction, verb handling. Many, fast, no network, no processes.
2. **Integration / e2e** (`cargo test --test '*'`): the real `kelir` binary driven against an
   in-process **scripted mock** OpenAI-compatible server (SSE, tool calls, usage with cached
   tokens, injected failures). Deterministic. Assert on `events.jsonl` and NDJSON stdout, on
   request headers/bodies the mock received, and on process behaviour (kill, resume, signals).
3. **Live drive** (done by the reviewer, not in CI): the release binary run in a Herdr pane
   against a real model via OpenRouter, the fixed task for the milestone, observed with `hc read`.
   Script: `scripts/live/` — commands that need keys read them from the environment only.

## Gates per milestone

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (all layers 1-2),
then reviewer sign-off from layer 3. Record the result in `docs/PROGRESS.md` (one row per
milestone: date, commit, test counts, live-drive verdict, known gaps).

## Linux gate

Everything that passes on the Windows dev box must also pass on Linux: run
`scripts/remote-check.sh <machine>` (the orchestrator tells you the machine name; it builds HEAD there
with fmt, clippy -D warnings and the full test suite). It is part of every milestone's gates. If you
add Unix-only code, it is only verified by that script, so run it before reporting.
