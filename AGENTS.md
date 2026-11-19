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

`bash scripts/gate.sh` (add `--linux=<machine>` to include the Linux half; the machine comes from the
flag or `KELIR_LINUX`, never from the repo) is the gate: it takes the one machine-wide
gate lock, caps itself at 4 build jobs and 4 test threads, runs `cargo fmt --check`, `cargo clippy
--all-targets -- -D warnings`, the whole suite through `cargo nextest run --no-fail-fast --retries 2 -j 4`,
`cargo test --doc`, and optionally `scripts/remote-check.sh`. One line per stage; `gate: PASS` or `gate:
FAIL`; logs in `<scratch>/gate-<job>-<stamp>/`. A test that needed a retry is named in the summary, and
`KELIR_GATE_STRICT_RETRIES=1` makes it fail the gate; the default keeps the gate green on the flakes STEER-9
still owns, so a known flake is visible but does not block an unrelated landing. `cargo test --lib <module>`
stays the fast loop; `scripts/gate.sh --quick` is fmt + clippy before a commit. Numbers and rejected
options: `docs/BUILD.md`.

Do not run the stages by hand in parallel with another worker's gate: the lock is what keeps three workers on
one desktop from flaking each other (STEER-9). If you must bypass it, `--no-lock` exists for a driver that
already holds it.

Then reviewer sign-off from layer 3. Record the result in `docs/PROGRESS.md` (one row per milestone: date,
commit, test counts, live-drive verdict, known gaps).

## Linux gate

Everything that passes on the Windows dev box must also pass on Linux. The normal way is through the gate:
`bash scripts/gate.sh --linux=<machine>` (the orchestrator tells you the machine name; the flag or
`KELIR_LINUX` is the only place it comes from — no machine name is stored in this repo). Commit before you
run it: the script bundles committed `HEAD`, so uncommitted work is not what gets verified. It builds HEAD
on the far side with fmt, clippy -D warnings and the full nextest suite into a persistent, reused work dir.

Name that work dir after your job — `kelir-verify-<job>`, as `gate.sh --linux` does — because it is a
reused clone with a warm `target/`, and each job keeps its own. `scripts/remote-check.sh prune <machine> [--job NAME] [--keep N] [--yes]` retires work dirs: it only ever deletes directories it
created (an on-disk marker), it keeps the newest N *per job*, and without `--yes` it is a dry run. Never
delete a work dir to make a gate pass.

If you add Unix-only code, only that run verifies it, so run it before reporting.

## No leaked processes, no visible windows (hard rule)

The dev box is someone's desktop. A test, script or benchmark that spawns processes must kill every
process tree it started, even when it fails or is killed (Drop guard / Job Object / `taskkill /T`).
Never launch unbounded numbers of jobs. No spawn path may create a visible console window on Windows:
child processes of a console-less parent each get their own window unless started with
`CREATE_NO_WINDOW`. Before reporting, check `tasklist` for leftover `kelir.exe`/`bash.exe` from your
runs. Reproduce Windows process bugs from a console-less parent too (not only from a Herdr pane,
which has a console). Incident: 2026-11-19, 160+ stalled `bg-shim` jobs and their consoles.

## Continuous driving never skips verification

The orchestrator drives M5-A through M11 without pausing for the maintainer's approval between
milestones. That is the only thing "continuous" removes. Every milestone and every steering job still
ends with: the gates on Windows **and** Linux (`cargo fmt --check`, `clippy -D warnings`, full
`cargo test`, `scripts/gate.sh --linux=<machine>`), then a **fresh reviewer's verdict** in `docs/reviews/`
(PASS, or fixes verified), then the retro pass, and only then the next one starts. A failing gate or a
FAIL verdict blocks the next item; it is fixed first. Each landed milestone is recorded in
`docs/PROGRESS.md`.

## Parallel work: worktrees, one writer per worktree

Concurrency is by **git worktree**: each job has its own worktree (`C:/Users/the maintainer/kelir-wt/<job>`),
branch `job/<job>`, target dir, state dir and Linux work dir (`bash scripts/gate.sh --linux=<machine>`, which passes
`kelir-verify-<job>`). The rule is *one writer per worktree*, and **only the orchestrator merges to
main**, one branch at a time: gates on the branch (Windows + Linux), a fresh reviewer on the branch, then
merge, then the gates again on main. Workers keep changes localized (new modules, surgical edits to shared
files) because several jobs edit `session.rs`/`config.rs`. Never kill processes by image name.

## Docs are part of done

Every job that adds or changes a config key, CLI flag, env var, verb, event, file location or TUI key updates
`docs/reference/` (see docs/tasks/STEER-11.md) in the same commit series; the drift test enforces it once it exists.

## Never broadcast console signals (hard rule)

Tests, scripts and tools must never send a console control event to process-group 0 or to a group they share with the
session that runs them (`GenerateConsoleCtrlEvent(_, 0)`, `kill(0, SIGINT)`, `kill -INT 0`): it is delivered to every process on
that console, including the kelir/agent session running the tests (it killed two dogfood sessions with exit 0xC000013A on
2026-11-19). Start the child under test in its own process group (`CREATE_NEW_PROCESS_GROUP` on Windows, `setsid`/`setpgid` on
Unix) and signal only that pid/group; if Windows console Ctrl-C delivery cannot be tested safely, cover it with a unit test of
the handler plus a Unix-only e2e.
