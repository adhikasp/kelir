# Implementation Plan: The `/` completion popup (commands and skills, fuzzy, Tab)

**Branch**: `feat/tui-slash-completion` | **Date**: 2026-11-28 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/010-tui-slash-completion/spec.md`

## Summary

One new module (`src/tui/complete.rs`) holds the whole feature: a `Catalogue` built once at TUI start
(the static `COMMANDS` plus the skills the session's own discovery found), a `score` for the fuzzy
match, a `context` that decides where a popup belongs, and a `Completion` that holds the ranked rows
and the highlight. The app owns three fields (`catalogue`, `completion`, `completion_off`), the input
path gains one intercept block and two helpers, the view gains `layout_with` and `render_completion`,
and the commands table gains one action (`Action::Skill`) that dispatches the **existing** `skills`
verb. No new dependency, no new session field, no new keg of state: the skills are resolved once by the
code path that already renders the system prompt.

Three decisions are worth the ink:

1. **The popup is a word, not a line.** It opens on the first column's `/` and follows the word under
   the cursor, and a space after a command other than `/skill` closes it forever. Argument values the
   TUI cannot know (model names, tool names, file paths) stay out of its way.
2. **The rows come from the discovery, not from a second listing.** `skills_available` runs on the
   session's `Places`, the same struct `render_system` uses, so the list can never offer a skill the
   context does not have. It costs one extra read of the skill files at start.
3. **`/skill` reuses the `skills` verb.** The verb already does the right thing (a `source` message at
   the tail, the frozen system prompt untouched, so the cache prefix survives); the command only
   decides whether to send a path or a builtin's text. The one behavioural addition is that an idle
   session applies it immediately, the way an immediate `/compact` already does.

## Technical Context

**Language/Version**: Rust 2024 edition, nightly (workspace pin in `rust-toolchain.toml`).

**Primary Dependencies**: ratatui 0.30, crossterm 0.29, serde_json — all already present. **No new
dependency.**

**Storage**: none. The catalogue is memory; `/skill` appends to the session log through the existing
verb.

**Testing**: unit tests in `src/tui/complete.rs` (scorer, context, accept) and in the app/keys modules;
`tests/it/tui_completion.rs` drives `App` directly and renders through ratatui's `TestBackend`;
`tests/it/tui_pty_completion.rs` drives the real binary in a PTY and reads the screen through
`tests/it/tui_screen.rs`, a small escape-sequence emulator (cursor addressing, erase, SGR) so the
assertions are about what a person saw, not about the bytes a diffing repaint happens to write.

**Target Platform**: Linux, macOS, Windows (CI on all three). No platform-specific code.

**Project Type**: single Rust CLI crate.

**Performance Goals**: no measurable change. The catalogue is built once; a keystroke re-scores a few
dozen names with a greedy walk each. The popup takes at most eight rows from the transcript.

**Constraints**: `struct Session` stays at 15 fields; every `src/` file ≤ 800 lines before its
`#[cfg(test)]`; the embedded reference stays inside its 144 KB budget (it is exactly at the cap, so
`tui.md` was compressed where the popup's section was added); no `Command::new` outside
`src/sys/runner.rs`.

**Scale/Scope**: 3 user stories, 1 new source module, 5 source files touched surgically, 3 new test
files, docs in `docs/reference/{tui,sources-and-skills,source-map}.md` and `docs/SPEC.md`.

## Constitution Check

*GATE: must pass before Phase 0; re-check after Phase 1.*

- **One writer per worktree** — yes: `~/Workspace/kelir-wt/slash`, branch
  `feat/tui-slash-completion`, its own target dir.
- **Spec first** — `specs/010-tui-slash-completion/` ships with the change; `docs/SPEC.md` gains the
  sentence its TUI section was missing, and `specs/005-tui/spec.md` (the TUI's own spec) gains the Tab
  row and the `skill` command.
- **Feature boundary** — `src/tui/` is the view, and the popup is view state; the one thing that leaves
  it is a verb the core already implements. `Session` gains no field.
- **Shape** — `complete.rs` is ~340 lines before its tests; the files it touches stay under 800.
- **Docs are part of done** — a new TUI key (Tab), a new command (`/skill`) and a new behaviour all
  get their rows in `docs/reference/`, and the drift test (`cargo test --lib docs`) reads `COMMANDS`.
- **Proof** — before/after release binaries recorded in a PTY with the same keystrokes, and the new PTY
  test proven to fail on the base commit.

## Project Structure

### Source

```text
src/tui/complete.rs        NEW   Entry, Catalogue, Context/context, Completion, score
src/tui/keys.rs                  Press::Complete on Tab
src/tui/app/input.rs             intercepts, refresh_completion, accept_completion
src/tui/app/mod.rs               catalogue/completion/completion_off, set_skills, rows
src/tui/editor_impl.rs           completion_point, replace_before_cursor
src/tui/view.rs                  layout_with, POPUP_ROWS, COMPLETION_MARK, render_completion
src/tui/commands.rs             Action::Skill, the `skills` verb, `/skill` row
src/tui/mod.rs, terminal.rs      carry the skills into the app
src/core/session/{boot,util}.rs  skill_catalogue() on the session's Places
src/core/session/mainloop.rs     an idle `skills` verb applies now
src/features/sources/source/discover.rs  skills_available()
src/tui/app/entry.rs             a source row shows its path (or "in context")
```

### Tests

```text
tests/it/tui_completion.rs       App-level: context, ranking, Tab, Esc, layout rows (13)
tests/it/tui_pty_completion.rs   the real binary: draw, Tab, Esc (fails on base)
tests/it/tui_screen.rs           the escape-stream screen emulator the PTY test reads
```

## Phases

1. **Catalogue and scoring** (`complete.rs` + unit tests): the pure part, provable without a terminal.
2. **Wiring** (keys → input → editor → app → view) with the `App`-level tests.
3. **`/skill`** (command → verb → idle path) and the source row's path detail.
4. **PTY proof** (`tui_screen.rs`, `tui_pty_completion.rs`), then the base-commit run.
5. **Docs** within the budget, `scripts/gate.sh`, screenshots, PR.

## Risks

- **The docs budget is at the cap.** Adding the Completion section meant compressing other `tui.md`
  passages; the total is exactly 147456 bytes, and the unit test refuses a byte more. Any further
  paragraph needs a matching cut.
- **A PTY test that reads a diffing screen.** The escape stream is not a picture; the emulator
  (`tui_screen.rs`) is the thing that makes the assertion honest, and it is also what the before/after
  screenshots use. If a future ratatui upgrade changes the erase strategy, the emulator is what has to
  catch up, and a wrong replay would show up as a garbled `.txt` in the PR — which is why the PR carries
  the rendered screen, not only a green tick.
- **Tab is a bare control byte (`0x09`).** Nothing upstream claims it (no tab stop in the editor), but a
  terminal in a mode that eats Tab for completion would. Herdr passes it through; the PTY test sends the
  raw byte, which is what a person's terminal does too.
