# Implementation Plan: TUI word editing, `/new`, and a built-in `timebox` gate

**Branch**: `feat/tui-keys-new-timebox` | **Date**: 2026-11-28 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/009-tui-keys-new-timebox/spec.md`

**Note**: This template is filled in by the `/speckit-plan` command; its definition describes the execution workflow.

## Summary

Three independent slices of the shipped harness:

1. **Word keys.** `src/tui/keys.rs` already maps `Ctrl-H` (`0x08`), `KeyCode::Backspace`+CONTROL (kitty
   `CSI 127;5u`) and `Ctrl-Left/Right`/`Alt-Left/Right` to `Key::KillPrevWord` / `Key::WordLeft|WordRight`.
   What is missing is the spellings other terminals and Herdr actually send: `Ctrl-W` (`0x17`), the
   `Alt-b` / `Alt-f` pair that Herdr's own keymap emits for `ctrl-left` / `ctrl-right`, and a documented,
   tested contract for all of them. The editor's word helpers already treat a chip as one element.

2. **`/new`.** A TUI command that is not a verb: it ends the session (the existing `end` verb, which stops
   running calls first) and asks the process to restart. `main` re-execs the same binary with the same
   argv minus `--resume`, so the new session inherits cwd, config, model, provider and step cap without a
   single new `Session` field.

3. **Built-in `timebox` gate.** A stateful built-in at `turn_end`, exactly like `hint` is at
   `prompt_submit`: it reads the turn's opening human prompt and the log's own timestamps, and denies an
   early finish with steering. The kernel's "one `turn_end` deny per turn" rule is generalised to a
   per-gate budget (`max_denies`, default 1) so a 30-minute box is actually enforced, while every
   existing gate keeps today's behaviour.

## Technical Context

**Language/Version**: Rust 2024 edition, nightly (workspace pin in `rust-toolchain.toml`).

**Primary Dependencies**: crossterm 0.29 (key decoding), ratatui 0.30 (TUI), `time` 0.3 (already a
dependency: RFC 3339 timestamps for the window maths), `toml`, `serde_json`. **No new dependency.**

**Storage**: the session's append-only `events.jsonl`. The timebox window is *derived* from the log
(the opening `user` event's `ts`), never stored as session state.

**Testing**: unit tests in-module; e2e in `tests/it/` against the scripted mock provider
(`tests/it/gates_timebox.rs`) and in a real PTY (`tests/it/tui_new.rs`, `tests/it/tui_keys_word.rs`).

**Target Platform**: Linux, macOS, Windows (CI on all three).

**Project Type**: single Rust CLI crate.

**Performance Goals**: no measurable change; the timebox gate does one pass over the in-memory events at
`turn_end`, and the key mappings are one match arm each.

**Constraints**: `struct Session` stays at 15 fields; every `src/` file ≤ 800 lines before its
`#[cfg(test)]`; the embedded reference stays inside its size budget; no `Command::new` outside
`src/sys/runner.rs` and the bg shim.

**Scale/Scope**: 3 user stories, ~6 source files touched, 1 new source file (`builtin/timebox.rs`),
3 new test files, docs in `docs/reference/{tui,gates}.md` and `docs/SPEC.md`.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| principle | how this plan keeps it |
|---|---|
| I. Small footprint | no dependency added; `/new` reuses the process image instead of holding two sessions in memory |
| II. Durable by construction | `/new` ends the old session through the normal `end` verb, so its log is closed and `session_ended` recorded; the timebox window is recomputed from the log, so a resumed session measures from recorded facts |
| III. Freedom to the model | the timebox is a gate (user-configurable, off unless declared); nothing about it goes into the core loop |
| IV. Core knows no feature | all three changes live in `tui/` and `features/gates/`; the only core touch is a return value (`u8` sentinel) in `core/session/boot.rs` and `main.rs`'s dispatch, no new `Session` field |
| V. Scriptable and observable | `/new` is TUI-only by design (a restart of the process, not a session verb); the timebox is a declared gate, so `kelir docs gates` and `observe` cover it; every deny is a `gate` event |
| VI. Three operating systems | the re-exec uses `sys::runner::std_command` (the one cross-platform command builder); no path or API is Unix-only; the key mapping is pure crossterm |
| VII. Test pyramid | unit tests (mapping, duration parse, window maths), e2e (mock provider deny→continue, PTY `/new`), and the gate script |

## Project Structure

### Documentation (this feature)

```text
specs/009-tui-keys-new-timebox/
├── spec.md              # this feature's specification
├── plan.md              # this file
└── tasks.md             # phase 2 output (/speckit-tasks)
```

### Source Code (repository root)

```text
src/
├── main.rs                          # re-exec on the restart sentinel
├── core/session.rs                  # `tui()` doc + RESTART const export
├── core/session/boot.rs             # capture the TUI's restart flag, return the sentinel
├── tui/
│   ├── keys.rs                      # Ctrl-W, Alt-b / Alt-f; unit tests for every spelling
│   ├── commands.rs                  # Action::New, /help row
│   ├── app/mod.rs                   # App.restart
│   ├── app/input.rs                 # /new → end verb + restart + quit
│   └── mod.rs                       # run() returns whether this was a /new exit
└── features/gates/
    ├── builtin/mod.rs               # timebox in NAMES, events, options
    ├── builtin/timebox.rs           # NEW: window maths, duration parse, steering
    ├── config.rs                    # built-in option validation (via builtin::*)
    ├── decision.rs                  # `max_denies` on the answer; Verdict::Deny carries it
    ├── runner.rs                    # construct the timebox built-in
    └── feature.rs                   # per-turn deny budget

tests/it/
├── gates_timebox.rs                 # NEW: e2e deny → continue → done
├── tui_keys_word.rs                 # NEW: PTY, every spelling edits a real buffer
└── tui_new.rs                       # NEW: PTY, /new yields two sessions in one cwd/model
```

**Structure Decision**: the existing layout already separates the loop (`core/`), the features and the
TUI; every change lands in the module that owns the behaviour today.

## Phase 0 — Research (decisions taken)

1. **Which byte is Ctrl-Backspace?** Legacy terminals send `0x08`; the kitty protocol sends
   `CSI 127;5u` (crossterm: `Backspace` + CONTROL); Herdr's shipped keymap sends `0x17` for
   `ctrl-backspace` and `ESC b` / `ESC f` for `ctrl-left` / `ctrl-right`. Verified by reading
   `crossterm-0.29.0/src/event/sys/unix/parse.rs` (control table at `b'\x01'..=b'\x1A'`, `parse_csi`
   for `ESC[1;5D`, `parse_csi_u_encoded_key_code` for `127;5u`) and the operator's Herdr app keymap.
   Decision: map all of them; `Ctrl-W` is free (nothing claims it today).

2. **How does `/new` restart?** In-process session rebuild would mean a second `Session` construction
   path plus a terminal teardown inside the loop. Re-exec is smaller and matches the "one process, one
   log" invariant: the old process exits with the terminal restored, the new one starts clean. The
   restart travels as one `u8` sentinel from `core/session::tui` to `main`, never as a `Session` field.

3. **Where does the timebox window start?** From `LogEvent.ts` of the first human `user` event after
   the last `turn_ended`. Reading the log keeps resumed sessions honest and needs no core change; a
   `say` from a person mid-turn cannot reset the clock.

4. **Why `max_denies`?** The kernel blocks at most once per turn (spec 004 FR-007), which would accept
   an early finish after a single nudge. A per-gate budget in the *feature* (not the core) lets the
   timebox ask for more while every other gate, and any external gate, keeps the default of one.

5. **What opens a window by default?** `duration` from the gate's config; `only_when_asked = true`
   limits enforcement to turns whose prompt states a duration ("you have 30 minutes"), which is what the
   operator asked for on a box where most turns are quick questions.

## Phase 1 — Design

### State and data

- **`Press` / `Key`** (existing): no new variants; only more `KeyEvent` spellings map onto them.
- **`App.restart: bool`**: set by `/new`, read once when the TUI loop exits.
- **`GateAnswer.max_denies: u32`** (default 1): how many `turn_end` denials this gate may use in one
  turn; serialised in the answer JSON so an external gate can use it too.
- **`Timebox`** (built-in instance, kept per gate name across events):
  `{duration: Option<Duration>, steering: String, only_when_asked: bool, max_denies: u32,
  denied: Option<u32>}` — `denied` remembers the turn already nudged so the gate stays inside its
  budget and never repeats identical steering.

### Contracts

- Config: `[gates.<name>] builtin = "timebox"`, `event` (only `turn_end`), `duration` (`"90s"`,
  `"15m"`, `"2h"`, `"1h30m"`, or a positive integer of seconds), `steering` (string with `{elapsed}`,
  `{target}`, `{left}`), `only_when_asked` (bool), `max_denies` (positive integer, ≤ 50).
- Gate answer: `{"decision": "deny", "reason": "…", "max_denies": N}`.
- Exit: `kelir` returns `75` from an interactive session that ended through `/new`; `main` re-execs
  itself in that case and passes the child's own exit code through.

### Quickstart validation

```bash
# 1. keys
cargo test --lib tui::keys
scripts/gate.sh --quick

# 2. /new (real PTY, scripted provider)
cargo nextest run -E 'test(/)' tui_new

# 3. timebox
cargo nextest run -E 'test(/)' gates_timebox
kelir docs gates | sed -n '/timebox/p'
```

## Complexity Tracking

No constitution violation. The one rule this feature *changes* is spec 004's "at most once per turn":
it becomes "at most `max_denies` (default 1)", and `specs/004-gates-and-hooks/spec.md` is updated in the
same change, as the constitution requires.
