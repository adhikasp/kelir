# Tasks: The `/` completion popup (commands and skills, fuzzy, Tab)

**Input**: Design documents from `/specs/010-tui-slash-completion/`

**Prerequisites**: plan.md, spec.md

**Tests**: the constitution makes the pyramid non-negotiable (VII): unit for the scorer and the key
map, an in-app render test for the layout, and a PTY test for the real binary — the PTY test must fail
on the base commit.

**Organization**: by user story, so each lands and is verifiable on its own.

## Phase 1: Setup

- [x] T001 Branch `feat/tui-slash-completion` at `origin/main` e6f97ed in its own worktree; build warm.

## Phase 2: User Story 1 — the list and Tab (P1)

- [x] T002 `src/tui/complete.rs`: `Entry` (label, name, about, insert, kind, path), `Catalogue`
  (commands from `COMMANDS`, skills from `SkillInfo`), `Context`/`context()` (first column, end of word,
  `/skill` argument), `Completion` (rows, selected, `move_by`, `chosen`, `Entry::accept`).
- [x] T003 `src/tui/keys.rs`: `Press::Complete` on `KeyCode::Tab`; `src/tui/app/input.rs`: intercept
  Complete / Esc / Up / Down while open, `refresh_completion` (re-query per edit, close on no rows, keep
  the highlight by label, Esc holds until the buffer changes), `accept_completion`.
- [x] T004 `src/tui/editor_impl.rs`: `completion_point()` (whole buffer + cursor) and
  `replace_before_cursor(start, text)` in characters.
- [x] T005 `src/tui/app/mod.rs`: `catalogue`, `completion`, `completion_off`, `set_skills`,
  `completion()`, `completion_rows()`.
- [x] T006 `src/tui/view.rs`: `layout_with(area, editor_rows, popup)`, `POPUP_ROWS = 8`,
  `COMPLETION_MARK = "▸ "`, `render_completion` (one description column, windowed on the highlight, mark
  first).
- [x] T007 `src/core/session/boot.rs`, `util.rs`, `src/tui/mod.rs`, `src/tui/terminal.rs`: resolve the
  skills once (`skills_available` on the session's `Places`) and hand them to the app.
- [x] T008 [P] `tests/it/tui_completion.rs` (in-app), `tests/it/tui_screen.rs` (escape-stream emulator)
  and `tests/it/tui_pty_completion.rs` (real binary: `/s` draws the list, Tab fills `/stop`, Esc
  dismisses and keeps the line).
- [x] T009 `docs/reference/tui.md`: areas row, Tab/↑/↓ keys, the Completion section.

## Phase 3: User Story 2 — fuzzy ranking (P2)

- [x] T010 `src/tui/complete.rs`: `score()` — greedy subsequence, first-char / word-start / camel /
  adjacency bonuses, capped gap penalty, exact > prefix > initials; ties by catalogue order.
- [x] T011 Unit tests: `mfc` → the long skill by initials, `cmp` → `/compact`, exact over prefix, prefix
  over scattered, no match closes.

## Phase 4: User Story 3 — `/skill` (P2)

- [x] T012 `src/tui/commands.rs`: `Action::Skill { name, path }`, verb `skills`, args (`path`, or
  `text` for a builtin), echo, the `/help` row, `/skill` with no name = usage line.
- [x] T012a `src/tui/app/input.rs` `submit`: a bare `/skill <name>` takes its `path` from the
  catalogue row (the verb runs no discovery), so a discovered skill loads instead of coming back a
  usage error; a builtin still travels as `text`. Tests: `a_discovered_skill_is_sent_with_the_path_the_list_showed`,
  `a_builtin_skill_is_sent_as_its_text`.
- [x] T013 `src/core/session/mainloop.rs`: apply `Verb::Skills` immediately in the idle loop (nothing
  running, no boundary); a failure is a `» skill: …` note.
- [x] T014 `src/tui/app/entry.rs`: a source row shows the path it came from (or `in context` for a
  spilled inline body), not a kilobyte of prose.
- [x] T015 `docs/reference/{sources-and-skills,source-map}.md`, `docs/SPEC.md`: `/skill`, the popup, the
  new module row.

## Phase 5: Proof and polish

- [x] T016 Prove the PTY test fails on the base commit ("the popup never drew"), then passes here.
- [x] T017 Before/after screenshots of the **release** binaries, same keystrokes, from a PTY replay.
- [x] T018 `env -u RUSTUP_TOOLCHAIN bash scripts/gate.sh`: fmt, shape, clippy, nextest, doctest.
- [x] T019 Open the MR with the screenshots attached and the failing assertion pasted.
