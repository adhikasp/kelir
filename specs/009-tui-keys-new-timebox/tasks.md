# Tasks: TUI word editing, `/new`, and a built-in `timebox` gate

**Input**: Design documents from `/specs/009-tui-keys-new-timebox/`

**Prerequisites**: plan.md (required), spec.md (required for user stories)

**Tests**: The repo's constitution makes the test pyramid non-negotiable (VII), so every story carries
unit, e2e and — where it is a keystroke — PTY coverage.

**Organization**: Tasks are grouped by user story, so each lands and is verifiable on its own.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (US1, US2, US3)

## Path Conventions

Single Rust crate: `src/`, `tests/it/` at the repository root.

---

## Phase 1: Setup

- [x] T001 Confirm the branch (`feat/tui-keys-new-timebox`) is at `origin/main` and the build is warm (`cargo build --tests`).

## Phase 2: User Story 1 — word keys (P1)

- [x] T002 [US1] `src/tui/keys.rs`: map `Ctrl-W` (`0x17`) to `Key::KillPrevWord`, and `Alt-b` / `Alt-f` to `Key::WordLeft` / `Key::WordRight`; document the byte spellings in the module comment.
- [x] T003 [US1] `src/tui/keys.rs`: extend the unit tests so every spelling (`Char('h')+CTRL`, `Char('w')+CTRL`, `Backspace+CTRL`, `Char('b')+ALT`, `Char('f')+ALT`, `Left/Right+CTRL`, `Left/Right+ALT`) asserts its `Press`.
- [x] T004 [US1] `src/tui/editor.rs`: prove (unit test) that `KillPrevWord` on an empty first line and `Ctrl-K` on an empty buffer are no-ops that never insert a control character.
- [x] T005 [P] [US1] `tests/it/tui_keys_word.rs`: PTY drive — type `alpha beta gamma`, send each word-delete spelling, assert the buffer; send Ctrl-Left twice, type `X`, assert the insert position.
- [x] T006 [US1] `docs/reference/tui.md`: key table gains Ctrl-W, Alt-B/F and the "any terminal" note; keep the docs size budget.

## Phase 3: User Story 2 — `/new` (P2)

- [x] T007 [US2] `src/tui/commands.rs`: `Action::New`, its `verb()`/`args()`/`line()`, `/help` row, parser arm.
- [x] T008 [US2] `src/tui/app/mod.rs` + `app/input.rs`: `App.restart`, and `/new` dispatches `end`, sets `restart`, then quits.
- [x] T009 [US2] `src/tui/mod.rs` + `core/session/boot.rs` + `core/session.rs` + `src/main.rs`: carry "ended by `/new`" out of the session and re-exec the binary without `--resume`.
- [x] T010 [P] [US2] `tests/it/tui_new.rs`: PTY — one turn, `/new`, assert two session dirs sharing cwd and model, and the old one ending `session_ended`.
- [x] T011 [US2] `docs/reference/tui.md` and `docs/SPEC.md`: `/new` documented (what it keeps, what it drops, the printed resume hint).

## Phase 4: User Story 3 — `timebox` built-in gate (P3)

- [x] T012 [US3] `src/features/gates/builtin/timebox.rs`: duration parsing (`90s`, `15m`, `2h`, `1h30m`, integer seconds), window from `ctx.events` timestamps, steering text with `{elapsed}`/`{target}`/`{left}`, `max_denies`.
- [x] T013 [US3] `src/features/gates/builtin/mod.rs`: `timebox` in `NAMES`, `default_event`/`supported_events`/`supports_event` (`turn_end` only), and `validate_options` (allowed keys, duration/steering/bools).
- [x] T014 [US3] `src/features/gates/decision.rs`: `max_denies` parsed from the answer (default 1) and carried on `Verdict::Deny`.
- [x] T015 [US3] `src/features/gates/runner.rs` + `feature.rs`: construct the built-in; per-gate deny budget replacing the single "blocked once" flag (default budget 1 keeps today's behaviour).
- [x] T016 [P] [US3] `tests/it/gates_timebox.rs`: e2e — early deny then continue then done; a late turn allowed; the config errors (bad duration, unknown key, wrong event).
- [x] T017 [P] [US3] `tests/it/gates_core.rs` (or the new file): the existing once-per-turn test still passes with the default budget.
- [x] T018 [US3] `docs/reference/gates.md`, `docs/SPEC.md`, `specs/004-gates-and-hooks/spec.md`: the built-in, its options, and the generalised deny budget.

## Phase 5: Polish

- [x] T019 `docs/reference/source-map.md`: row for `src/features/gates/builtin/timebox.rs`.
- [x] T020 `bash scripts/source-map-check.sh`, `bash scripts/shape.sh`, `env -u RUSTUP_TOOLCHAIN scripts/gate.sh` — all green.
- [x] T021 Open the MR; wait for CI on all three OSes; merge.
- [x] T022 Enable `[gates.timebox]` in the machine config on semar, reinstall the binary, verify with a live drive.
