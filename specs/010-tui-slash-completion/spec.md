# Feature Specification: The `/` completion popup (commands and skills, fuzzy, Tab)

**Feature Branch**: `feat/tui-slash-completion`

**Created**: 2026-11-28

**Status**: Draft

**Input**: User description: "can you implement an auto-complete box and its related behaviour (tab) if
i type `/` for commands and skills. it should also be able to do fuzzy search in an intuitive manner."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - See the commands the moment a `/` is typed (Priority: P1)

A person opens the prompt, types `/`, and a list appears above the editor: every `/` command with its
one-line description, then every skill this session discovered. Typing more letters narrows the list
(`/s` → `/stop`, `/say-now`, `/skill`). Tab types the highlighted row into the line and leaves the
cursor after it, ready for the argument.

**Why this priority**: The command list lives only in `/help` today, and `/help` costs a turn: it
prints into the transcript and pushes the answer the person was reading off the screen. A person who
half remembers a command has to recall it exactly or read a wall of text.

**Independent Test**: In a real PTY, type `/s`, assert the screen shows the command rows with their
descriptions and the `▸` mark on the first; send `\t` and assert the line reads `/stop` and the list is
gone.

**Acceptance Scenarios**:

1. **Given** an idle session, **When** `/` is typed in the first column, **Then** the list opens with
   the commands first (in `/help` order) and the skills after, at most eight rows, its first row marked
   `▸`, taking its rows from the transcript directly above the editor.
2. **Given** the list is open on `/s`, **When** Tab is pressed, **Then** the line becomes `/stop ` and
   the cursor sits after the space; the list closes.
3. **Given** the list is open, **When** ↑ / ↓ is pressed, **Then** the highlight moves and wraps at the
   ends, and the editor's history/cursor does not move.
4. **Given** the list is open, **When** Esc is pressed, **Then** the list closes, the line is unchanged,
   and no `stop` verb is sent; typing another character opens it again.
5. **Given** no list is open, **When** Tab is pressed, **Then** nothing happens (no literal tab is
   inserted).

---

### User Story 2 - Fuzzy match that ranks the way a person scans (Priority: P2)

The query does not need to be a prefix. `/cmp` finds `/compact`, `/mfc` finds
`monthly-finance-close`. A name that starts with the query beats one that merely contains it, adjacent
characters beat scattered ones, a word start beats the middle of a word, and an exact name beats a
prefix beats the initials.

**Why this priority**: A completion list you have to prefix-match is slower than `/help` for long names
(this machine's skills include `monthly-finance-close`).

**Independent Test**: Unit-test the scorer on the ranking order (`exact > prefix > initials > scattered`)
and on the subsequence walk, and assert the list order for a handful of queries.

**Acceptance Scenarios**:

1. **Given** the catalogue, **When** the query is `mod`, **Then** `/model` is the first row.
2. **Given** the catalogue, **When** the query is `mfc`, **Then** the `monthly-finance-close` skill is
   found by its initials.
3. **Given** the catalogue, **When** the query matches nothing, **Then** no list is drawn (an empty box
   would still take Tab, ↑/↓ and Esc and say nothing).
4. **Given** two rows of equal score, **When** the list is drawn, **Then** the catalogue order decides,
   so a keystroke never makes rows hop about.

---

### User Story 3 - Load a skill with `/skill`, after the same discovery (Priority: P2)

`/skill ` lists only the skills, by name, with their descriptions, and Tab types just the name (the
command is already there). Enter sends the `skills` verb with that name, and the skill's instructions
arrive in the context at the next boundary — or immediately when nothing is running. A skill compiled
into the binary travels as its text; a discovered one as its path; a name discovery missed can be given
a path (`/skill <name> <path>`).

**Why this priority**: Skills are *pull*: the prompt names the directories, and until now the only way
to pull one was `bash cat …`. `/skill` is the name a person would type, and the popup is how they find
it.

**Independent Test**: e2e — a session holding a project skill, `/skill <name>` in the TUI, then the next
provider request's messages carry the skill body once, as a `source` message at the tail; the frozen
system prompt is unchanged.

**Acceptance Scenarios**:

1. **Given** `/skill ` is typed, **When** the list opens, **Then** only skills are listed and Tab types
   `<name> `, not `/skill <name> `.
2. **Given** a skill from the session's own discovery, **When** `/skill <name>` is sent, **Then** it is
   applied as a `source` message at the next boundary (`next_step`), and immediately when idle.
3. **Given** a builtin skill (`kelir`, `delegation`, `herdr`), **When** it is chosen, **Then** its text
   goes with the verb (it has no file to read).
4. **Given** `/model ` or any other command's argument, **When** it is typed, **Then** no list opens.

---

### Edge Cases

- A second line makes the buffer a message, not a command: no list, however the first line starts.
- The cursor mid-word (`/modx` with the cursor after `mod`) completes nothing, because accepting would
  replace the wrong characters.
- A very long skill list: the window follows the highlight, so the highlighted row is always visible and
  the fixed rows (state, editor, facts) keep their height.
- A narrow pane: the popup takes at most the room above the editor; the editor keeps its rows.
- A description wider than the pane is cut at the right edge, so the `▸` mark is the first thing on the
  line, not the last.
- Scrolling the transcript or walking the editor history while the list is up does not re-open a
  dismissed list (Esc holds until the buffer changes).
- `/skill` alone (no name) is the usage line, never a verb with an empty name.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The TUI MUST open a completion list when the first line of the buffer starts with `/` in
  the first column and the cursor is at the end of a word of that line.
- **FR-002**: The list MUST offer every `/` command (in `/help` order) and every skill the session's
  own discovery found (sorted by name), each with its one-line description; an empty query lists the
  commands first.
- **FR-003**: The list MUST be at most eight rows, windowed to keep the highlighted row visible, drawn
  directly above the editor with its rows taken from the transcript so the fixed rows do not move.
- **FR-004**: The highlighted row MUST carry the `▸` mark first on the line; commands and skills MUST be
  distinguishable without colour alone (skills dim).
- **FR-005**: The match MUST be a case-insensitive greedy subsequence over the command or skill *name*,
  scored in this order: an exact name, then a prefix, then the initials; within a match, a first
  character, a word start, a camel hump, adjacency, and a capped gap penalty. Equal scores MUST keep
  catalogue order.
- **FR-006**: A query that matches nothing MUST NOT open a list.
- **FR-007**: Tab MUST accept the highlighted row: replace the typed word with what the row inserts and
  leave the cursor after it. A command inserts `/<name> ` — the name and a space, never the usage's
  placeholder. A skill inserts `/skill <name> `, or `<name> ` as `/skill`'s argument.
- **FR-008**: Tab with no list open MUST do nothing; it MUST never insert a literal tab.
- **FR-009**: ↑ / ↓ MUST walk the list (wrapping) while it is open, and MUST leave the editor's
  cursor/history handling alone while closed.
- **FR-010**: Esc MUST close the list and leave the buffer alone; the dismissal MUST hold until the
  buffer changes, then the list reopens as the new word is typed. With no list open, Esc keeps its
  meaning (stop, only while working).
- **FR-011**: `/skill ` (and only `/skill `) MUST narrow the list to skills; any other command's
  argument MUST open no list.
- **FR-012**: `/skill <name> [path]` MUST be a command that dispatches the existing `skills` verb with
  `name` and either `path` (a discovered skill, or a file the discovery missed) or `text` (a builtin),
  so the skill arrives as a `source` message at the tail and the frozen system prompt (and the
  provider's cache prefix) is untouched. A bare `<name>` that the catalogue lists MUST take its `path`
  from that catalogue row: the verb does not run discovery, so sending the name alone is a usage error
  and the keystroke is lost.
- **FR-013**: The `skills` verb MUST apply immediately when the session is idle (nothing running, no
  boundary to wait for), and at the next boundary otherwise; a failure MUST be a note, not a lost
  keystroke.
- **FR-014**: The `/skill` rows MUST come from the same discovery the system prompt was rendered from,
  so the list can never offer a skill the context does not have.
- **FR-015**: `docs/reference/{tui,sources-and-skills,source-map}.md`, `docs/SPEC.md` and
  `specs/005-tui/spec.md` MUST describe the popup, Tab and `/skill`, within the embedded reference's
  size budget.

### Key Entities

- **Entry**: one row — `label` (`/model <p/m>`, `/skill sift`), `name` (what is scored: `model`,
  `sift`), `about` (the description), `insert` (what Tab types), `kind` (command / skill), `path`.
- **Catalogue**: the entries, built once at TUI start (commands are static, skills come from
  `skills_available` on the same `Places` the prompt render used).
- **Context**: where the cursor is and what it completes — `query`, `start` (character index),
  `skill_argument` (the word is `/skill`'s first argument).
- **Completion**: the context plus the matching catalogue indices, best first, and the highlighted
  position.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: The real binary, in a PTY, draws the list for `/s` (with descriptions), and Tab turns the
  line into `/stop` — on the base commit the same test fails with "the popup never drew".
- **SC-002**: Every command in `COMMANDS` has a row whose accepted insert parses as that command
  (unit test), so Tab can never produce a line the parser rejects.
- **SC-003**: For a query matching nothing, no list is drawn and Tab inserts nothing.
- **SC-004**: `/skill <name>` puts the skill body in the next request's messages exactly once, after the
  human message, and nothing else changes.
- **SC-005**: `scripts/gate.sh` passes, no `src/` file exceeds 800 lines before its `#[cfg(test)]`, and
  the embedded reference stays within its budget.

## Assumptions

- The list is for the *word being typed*, not the whole line: a space after the command commits it, so
  `/model ` opens nothing. This is the rule that keeps the popup out of the way of arguments whose
  values the TUI cannot know (model names, tool names, paths).
- The catalogue is a snapshot at TUI start. A skill added mid-session is loadable by path, which covers
  the case without a second discovery pass on every keystroke.
- Skills are ordered by name because the prompt renders them that way; commands keep `/help` order
  because that is the order a person reads them in.
- Tab is free: the editor has no tab stop, and a literal tab in a prompt is not useful.
- Colour is decoration: the `▸` mark, the alignment and the dim/bright split carry the state under
  `NO_COLOR`.
