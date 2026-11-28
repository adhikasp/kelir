# Feature Specification: TUI word editing, `/new`, and a built-in `timebox` gate

**Feature Branch**: `feat/tui-keys-new-timebox`

**Created**: 2026-11-28

**Status**: Draft

**Input**: User description: "ctrl-backspace should delete a word; ctrl left/right arrow move word by
word. `/new` starts a new session and clears the existing one. A built-in gate for timeboxed work: a
prompt like 'research X, you have 30 minutes' makes the harness refuse an early stop and steer the
model to keep digging until the time is spent."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Word editing keys that work in every terminal (Priority: P1)

A person is typing a long prompt in the TUI and wants to fix one word. Ctrl-Backspace (and Ctrl-W)
delete the word before the cursor; Ctrl-Left / Ctrl-Right (and Alt-Left / Alt-Right, and Alt-H /
Alt-L) move the cursor one word at a time. It behaves the same inside a plain terminal, inside
Herdr, and through the kitty keyboard protocol, and it never leaves the prompt holding a stray
control character.

**Why this priority**: It is the only change that touches this machine's daily editing loop, and a
half-working key is worse than none: a key that deletes nothing but silently eats a word's worth of
input loses work.

**Independent Test**: Send each spelling of every key to the real binary in a PTY with a known
editor buffer and assert the resulting buffer text; assert each byte sequence a terminal can send
(legacy `0x08`, `0x17`, `ESC[127;5u`, `ESC[1;5D`, `ESC[1;5C`, `ESC b`) maps to the same action.

**Acceptance Scenarios**:

1. **Given** the editor holds `alpha beta gamma` with the cursor at the end, **When** Ctrl-Backspace is
   pressed, **Then** the buffer reads `alpha beta `.
2. **Given** the same buffer, **When** Ctrl-W is pressed, **Then** the buffer reads `alpha beta `.
3. **Given** the same buffer, **When** Ctrl-Left is pressed twice and then `X` is typed, **Then** the
   buffer reads `alpha Xbeta gamma`: two word jumps, then an insert at the cursor.
4. **Given** a legacy terminal that sends `0x08` for Ctrl-Backspace, **When** that byte arrives,
   **Then** it deletes a word, not one character.
5. **Given** the cursor is at the start of an empty prompt, **When** Ctrl-K is pressed, **Then** the
   editor is unchanged (no panic, no echoed control byte).
6. **Given** a chip (image) is the word before the cursor, **When** Ctrl-Backspace is pressed, **Then**
   the whole chip is removed and its file is deleted, as one element.

---

### User Story 2 - `/new` starts a clean session in the same place (Priority: P2)

Mid-session the person types `/new`. The current session ends cleanly (its log is closed and its turn
stopped), the screen is cleared, and a fresh session starts in the same working directory with the
same config, model and provider. The old session stays on disk and its id is printed so it can be
resumed.

**Why this priority**: Restarting today means quitting, retyping the command with the same flags, and
remembering the directory; `/new` is the missing half of `/quit`.

**Independent Test**: In a PTY, run one turn, type `/new`, and observe a new session id in the state
directory, the same cwd and model in the new banner, and two session directories after the run.

**Acceptance Scenarios**:

1. **Given** a session that has answered one turn, **When** `/new` is submitted, **Then** the old
   session's log ends with `session_ended` and a new session directory appears.
2. **Given** a session started in `~/project` with `--model X`, **When** `/new` is submitted, **Then**
   the new session's banner and `session_started` carry `~/project` and model `X`.
3. **Given** a running turn, **When** `/new` is submitted, **Then** the turn is stopped first, and the
   old session's log records the stop before `session_ended`.
4. **Given** `/new` in a resumed session (`--resume <id>`), **When** it runs, **Then** the new session
   does not resume the old log: it is a new id.
5. **Given** the new session's process cannot start, **When** `/new` runs, **Then** the person is told
   so and the terminal is left usable (no raw mode, no alternate screen).

---

### User Story 3 - A timebox gate that refuses an early stop (Priority: P3)

The person writes `research X. you have 30 minutes`. When the model decides it is done after five
minutes, the built-in `timebox` gate at `turn_end` refuses the answer once and sends back steering:
"you stopped after 5 of 30 minutes: dig deeper, check another angle, re-verify". The model keeps
working. Once the time has passed, the answer is accepted; a turn that finishes late closes the
window, so the next prompt starts fresh.

**Why this priority**: The other two are keystrokes; this one changes what a session does, and only
matters once the first two are safe.

**Independent Test**: Configure the gate with a short duration and a scripted mock provider that
answers early: assert the session makes a second request, the steering message is a `user` event from
`gate`, and the turn ends on the second answer.

**Acceptance Scenarios**:

1. **Given** a `timebox` gate with `duration = "30m"` and 5 minutes elapsed since the turn started,
   **When** the model ends its turn, **Then** the turn continues with the configured steering as a
   `user` message from `gate` and the `gate` event records `decision: deny`.
2. **Given** the same gate and 31 minutes elapsed, **When** the model ends its turn, **Then** the turn
   ends (`done`) and no steering is sent.
3. **Given** a turn that was denied early and then finishes after the duration, **When** the second
   `turn_end` fires, **Then** it is allowed and the window is
   recorded closed.
4. **Given** `duration` missing or unparseable, **When** the config is loaded, **Then** kelir refuses
   to start with a message naming the key and the accepted forms (`90s`, `30m`, `2h`).
5. **Given** a `[gates.timebox]` table with `builtin = "timebox"` and any other spelling of its
   options (e.g. `Duration`, `steering`), **When** the config loads, **Then** the unknown key is
   reported with the allowed list.
6. **Given** any other built-in's event, **When** `timebox` is bound to `session_start`, **Then** the
   config is refused: `timebox` supports only `turn_end`.
7. **Given** a session with the gate configured and a prompt that mentions no time, **When** the turn
   ends, **Then** it is allowed: a timebox is a property of the session/config, not of the prompt text.

### Edge Cases

- The gate's own message must not retrigger a timebox: it is sent `from: gate`, not from a person.
- A resumed session: the window starts at the resumed turn's first user message, not at log start.
- A turn with no user message (a `later` delivery only) never opens a window.
- Non-UTF-8 or huge config values: `duration` is a string or a positive integer of seconds.
- `/new` while the TUI's editor is mid-line: the draft is dropped with the session (it is not
  silently carried into the new one).
- The timebox clock uses the log's own timestamps (`ts` on each event), so a resumed or replayed log
  is measured from recorded facts rather than from wall-clock guesswork.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The TUI MUST map Ctrl-Backspace, Ctrl-H (`0x08`), the kitty `CSI 127;5u` / `CSI 8;5u`
  spellings and Ctrl-W (`0x17`) to "delete the previous word".
- **FR-002**: The TUI MUST map Ctrl-Left / Ctrl-Right, Alt-Left / Alt-Right, Alt-H / Alt-L and the
  CSI `1;5D` / `1;5C` / `1;3D` / `1;3C` spellings to "move one word left / right".
- **FR-003**: The editor MUST treat a chip as one word for both the delete and the motion.
- **FR-004**: The editor MUST NOT act on a key at an empty buffer where the action is a no-op, and MUST
  never insert a control character into the prompt.
- **FR-005**: `/new` MUST be a TUI command that ends the current session cleanly and starts a new
  session in the same cwd, with the same config, model, provider and step cap.
- **FR-006**: `/new` MUST print the ended session's id (so it can be resumed) before the new session
  starts, and MUST NOT resume that log.
- **FR-007**: `/new` MUST stop a running turn before ending the session.
- **FR-008**: `/new` MUST restart the process (no session state carried in memory) and MUST NOT pass
  `--resume` to it.
- **FR-009**: A new built-in gate named `timebox` MUST exist, declared as `[gates.<name>]` with
  `builtin = "timebox"`, default and only event `turn_end`.
- **FR-010**: `timebox` MUST accept options `builtin`, `event`, `duration`, `steering`,
  `only_when_asked` and `max_denies`; `duration` accepts `90s`, `15m`, `2h`, `1h30m` or a positive
  integer of seconds; `steering` is a string, with a documented default; `only_when_asked` is a
  boolean and `max_denies` a positive integer. Any other key is a config error listing the allowed
  ones.
- **FR-011**: `timebox` MUST compute the window start from the `ts` of the current turn's latest human
  message (one whose `from` is `tui`, `ctl`, `cli` or `channel`, after the last `turn_ended`) and the
  window length from the budget the prompt states (`duration` when it states none, unless
  `only_when_asked`).
- **FR-012**: While `elapsed < target`, `timebox` MUST answer `deny` with the steering text (`{elapsed}`,
  `{target}`, `{left}` filled in), sending the model back to work.
- **FR-013**: `timebox` MUST carry its own per-turn block budget (`max_denies`, default 10) so the clock,
  not a counter, ends the window; other gates keep the default of one. A completed late turn MUST
  record the window closed in the run's `reason`.
- **FR-014**: `timebox` MUST NOT deny a turn whose last user message is not from a person (`tui`,
  `ctl`, `cli`, `channel`) — a gate's own steering is never timeboxed again.
- **FR-015**: The TUI key table, the gates reference, the config reference (if a new key is added), the
  verbs/spec docs and the embedded skill MUST describe the three features, within the docs size
  budget.
- **FR-016**: The machine config on semar MUST enable the `timebox` gate so it applies to sessions
  started there (an out-of-repo change, documented in the operator's own notes).

### Key Entities

- **Editor buffer**: lines, cursor row/column, chips. A "word" is a whitespace-separated run; a chip is
  one indivisible element.
- **`Press`** (the TUI's translated key): `Edit(Key)` carries the editor action.
- **`GateDef` / `builtin`**: a declared gate, its event, and its options table.
- **The turn window**: `{start_ts, duration, closed}` — derived from log timestamps, not stored in a
  session field.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: All six keystroke spellings for word-delete and word-motion reach the same editor action;
  a unit test per spelling proves the mapping and a PTY test proves the buffer effect.
- **SC-002**: `/new` produces exactly two session directories, and the second one carries the first
  one's cwd and model.
- **SC-003**: With a 1-second timebox and an early answer, a session makes exactly two provider
  requests and the second request's messages contain the steering text once.
- **SC-004**: `scripts/gate.sh` passes with no new `shape-allow.txt` exception and no file over 800
  lines before its `#[cfg(test)]`.
- **SC-005**: The reference's embedded size stays within its budget, and `kelir docs --check` is clean.

## Assumptions

- The person's terminal sends Ctrl-Backspace as `0x08` (Herdr's own keymap sends `0x17` for
  `ctrl-backspace`), so both spellings are mapped, and neither is claimed by another action today.
- `/new` may restart the process: the TUI is a process-per-session design already, and the session is
  resumed from a log by id when needed.
- The budget is the prompt's when the prompt states one, because that is how the person asks for it
  ("you have 30 minutes"); `duration` in the config is the fallback, and `only_when_asked` turns the
  fallback off. The parser only reads a phrase that is a clock (short unit form, or one of "you have",
  "you've got", "timebox", "within" before it), so conversation that merely mentions a duration is not
  timeboxed, and the window is measured from log timestamps rather than trusted from the model.
- The gate may use the session's read-only event log (`GateCtx.events`) for timestamps, exactly as the
  built-in `hint` gate reads it.
- Registry-free: no new dependency is needed for duration parsing.
