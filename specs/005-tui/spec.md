# Feature Specification: Interactive TUI (transcript, state, editor, slash commands, markdown, scrollback)

**Feature Branch**: `005-tui`

**Created**: 2026-11-27

**Status**: Reverse-engineered from main e1cd8ea

**Input**: User description: "Reverse-build the spec of kelir's full-screen interactive terminal UI: src/tui, slash commands, editor, markdown rendering, scrollback."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Talk to the agent and watch it work (Priority: P1)

A person runs `kelir` with no subcommand in a terminal. The program takes over the alternate screen and shows four areas: a transcript, a one-row state line, a growing editor, and a one-row facts line. The person types a message and presses Enter. The prompt appears in the transcript, the state row animates (thinking, writing, running a tool), tool calls appear as one dim line each, and the assistant's answer appears as rendered markdown.

**Why this priority**: This is the product's primary interactive surface; without it nothing else here has value.

**Independent Test**: Drive the real binary in a pseudo-terminal against a scripted provider: type a prompt, press Enter, observe prompt echo, state transitions, a tool line and the answer, then return to `● idle · ready`.

**Acceptance Scenarios**:

1. **Given** an idle session, **When** the person types text and presses Enter, **Then** the text is echoed in the transcript in the user colour as `› text`, a `say` verb is sent to the session, and the editor empties.
2. **Given** a turn is running, **When** the person presses Enter with text, **Then** the text is sent as a `say` and the editor prompt is dimmed while the agent works.
3. **Given** a tool call settles, **When** the log event arrives, **Then** one line is appended with the call's head on the left and a right-aligned verdict (output size; `✗ exit N` on failure; `⏹ cancelled` when stopped).
4. **Given** the turn ends `done`, **When** the end event arrives, **Then** the state row returns to `● idle · ready` and no outcome line is written.
5. **Given** the turn ends `paused`, `error` or `stopped`, **When** the end event arrives, **Then** one outcome line (`⏸ paused · 3 steps · 4.8k tok`, etc.) is written, and for paused/error the state row keeps its text with the next action (`say continue` / `say to retry`).

---

### User Story 2 - Control a running turn with keys and slash commands (Priority: P1)

While the agent works, the person can stop it (Esc, Ctrl-C, `/stop`), redirect it (`/say-now`), queue a follow-up (`/later`), swap model (`/model`), toggle tools (`/tools`), compact context (`/compact`), inspect output (`/expand`, Ctrl-O), or quit (`/quit`, double Ctrl-C).

**Why this priority**: Being able to interrupt and steer is essential to trust the agent.

**Independent Test**: Parse each slash line and assert the verb name and arguments produced; press each key in a PTY and observe the resulting verb/echo.

**Acceptance Scenarios**:

1. **Given** a working turn, **When** the person presses Esc, **Then** `» stop (Esc)` is printed and a `stop` verb is sent; **Given** an idle session, **When** Esc is pressed, **Then** nothing happens.
2. **Given** any state, **When** Ctrl-C is pressed once, **Then** `» stop (Ctrl-C again within a second to quit)` is printed and `stop` is sent; **When** pressed a second time within 1 s, **Then** `» quit (second Ctrl-C)` is printed and `end` is sent.
3. **Given** `/say-now fix it`, **When** submitted, **Then** the line `› fix it  (now)` is echoed and a `say` verb with `now: true` is sent.
4. **Given** `/tools off bash`, **When** submitted, **Then** a `tools` verb with `disable: ["bash"]` is sent and `» tools off bash` is echoed; `/tools on edit` sends `enable: ["edit"]`.
5. **Given** `/expand 3` (or `/expand last`), **When** submitted, **Then** the output of call #3 (or the last call) is appended below as dim rows, capped at 40 rows with a "N more rows" note, followed by a note that a committed line cannot unfold.
6. **Given** an unknown command `/frob`, **When** submitted, **Then** `kelir: unknown command `/frob` (try `/help`)` is shown and no verb is sent.

---

### User Story 3 - Read and navigate the transcript (Priority: P2)

Every prompt and answer stays reachable by scrolling inside the app. PageUp/PageDown scroll by a viewport, Home/End jump to the top/bottom. When output arrives while the reader is scrolled away, the state row shows `N new lines · End follows` and the view does not jump.

**Why this priority**: The terminal's own scrollback is deliberately unused, so in-app scrolling is the only way to read history.

**Independent Test**: Fill the transcript past one screen, press PageUp, append output, assert the viewport row is unchanged and the new-lines marker counts the added rows; press End and assert the marker clears.

**Acceptance Scenarios**:

1. **Given** the viewport follows the tail, **When** new rows are appended, **Then** the view stays at the bottom.
2. **Given** the person scrolled up, **When** new rows arrive, **Then** the visible rows do not move and the pending count increases.
3. **Given** the person scrolled up, **When** the terminal is resized, **Then** every row is rebuilt from its source entry at the new width and the reader keeps the same fractional position in the transcript.
4. **Given** scrolling reaches the bottom by PageDown, **When** the last row is visible, **Then** following the tail resumes and the pending count resets.
5. **Given** the TUI exits, **When** the alternate screen is left, **Then** the terminal's own scrollback is unchanged and `resume: kelir tui --resume <id>` is printed.

---

### User Story 4 - Rendered markdown that streams (Priority: P2)

Assistant text is rendered as styled rows (headings, emphasis, inline and fenced code, lists, task boxes, quotes, tables, links, rules, raw HTML as text) and appears progressively while streaming: finished lines of a long code block or finished top-level list items are committed to the transcript while only the last three lines stay live.

**Why this priority**: Answers are the main thing the person reads; long ones must not appear as a three-row ticker.

**Independent Test**: Render fixtures at several widths and compare row text and tones; stream a 60-line fenced block in deltas and assert rows are committed before the closing fence.

**Acceptance Scenarios**:

1. **Given** `# One`, **When** rendered, **Then** the row is `One` with heading tone (no `#`).
2. **Given** `- one\n- two\n  - nested`, **When** rendered, **Then** rows are `• one`, `• two`, `  ◦ nested`.
3. **Given** a table and width ≥ 30, **When** it fits, **Then** it is drawn boxed; **Given** width < 30 or it does not fit, **Then** rows are `a | b | c`.
4. **Given** a fenced block with a language label, **When** wider than the row, **Then** lines are clipped, not wrapped, with a gutter.
5. **Given** `NO_COLOR` is set non-empty, **Then** colours are dropped but bold/italic/underline and glyphs stay; **Given** `KELIR_ASCII` non-empty or `TERM=dumb`, **Then** state, bullet, gutter, quote and table-border glyphs and the spinner use ASCII.
6. **Given** a streamed answer is then finalised by the assistant event, **When** it lands, **Then** the finished text replaces the draft rows and the answer never appears twice.

---

### User Story 5 - Compose multi-line prompts and attach images (Priority: P2)

The editor supports multi-line input, word-wise movement and deletion, history, paste, and image attachment: pasting or typing the path of an image file turns it into a `[image N]` chip that sends a pointer to the model.

**Why this priority**: Rich input is expected but the agent is usable without it.

**Independent Test**: Unit-test the editor on key sequences; in a PTY paste an image path and assert a chip appears and the submitted `user` event carries the pointer.

**Acceptance Scenarios**:

1. **Given** text in the editor, **When** Alt-Enter, Shift-Enter or Ctrl-J is pressed, **Then** a newline is inserted instead of sending.
2. **Given** a long line, **When** it exceeds the editor width, **Then** it wraps onto further rows (prompt glyph on the first row only) and the editor grows up to 12 rows, scrolling to keep the cursor visible.
3. **Given** the cursor on the first visual row, **When** Up is pressed, **Then** the previous submitted message is loaded; Down on the last row moves forward and finally restores the unsent draft.
4. **Given** a bracketed paste of the path to a real image, **When** received, **Then** a chip `[image 1]` is inserted, the file is copied to `<session>/attachments/<n>-<name>.<ext>`, and on send the chip is replaced by `[image: attachments/…, WxH, size — run: kelir view …]`.
5. **Given** a chip at the cursor, **When** Backspace is pressed, **Then** the whole chip and its saved file are removed; plain Backspace elsewhere deletes one character.
6. **Given** a text file named `fake.png`, **When** its path is pasted, **Then** no chip is made.

---

### User Story 6 - Know the session's state at a glance (Priority: P3)

The state row shows glyph, phase word, elapsed time and detail; the facts row shows the session title, the model and input tokens; the terminal/Herdr pane title and status word follow the session.

**Why this priority**: Orientation aid; the session works without it.

**Independent Test**: Feed log events to the app and assert the state-row and facts-row text for each phase.

**Acceptance Scenarios**:

1. **Given** a step starts, **Then** the row reads `⠹ thinking 12s · waiting for the provider · esc stops` (spinner glyph varies).
2. **Given** text streams, **Then** the row reads `writing 7s · ~214 tok · esc stops` (tokens estimated at four characters each).
3. **Given** two tools run, **Then** the row reads `running 2 tools 12s · <preview> · esc stops`.
4. **Given** a retryable provider wait, **Then** the state row shows `rate limited · 429 · retrying in 34 s · attempt 5 · esc stops` and the facts row shows `kelir working · … · 33 s left`, with `say stop to cancel` rewritten to `esc stops`.
5. **Given** a `session_title` event, **Then** the title appears first in the facts row and a terminal-title record is emitted.
6. **Given** a narrow pane, **Then** the facts row drops the model, then falls back to `title · N tok in`, then clips with an ellipsis.

---

### Edge Cases

- Empty or whitespace-only Enter is ignored; a bare `/` shows `no command after `/``.
- `/say-now`, `/later`, `/model` with no text show `/<command> needs something after it`; `/tools` with no mode shows usage; `/tools off` without a name errors.
- `/expand 0` or `/expand abc` is rejected; `/expand` with no argument is rejected with the usage message; `/expand n` for an unknown call prints `kelir: no call #n`; with no calls yet, `kelir: no tool call to expand yet`.
- Ctrl-O with no tool output shows `no tool output yet`; pressing Ctrl-O again closes the overlay.
- Ctrl-Y with no answer sets note `no answer yet`; Ctrl-Shift-Y without a session dir sets note `no session dir to save the transcript`.
- At most 8 images per prompt; the 9th is refused with a note.
- A verb the session rejects produces a red `kelir: <reason>` line and puts the state row in error.
- A reasoning-only step (no assistant text) does not insert a blank row between tool lines.
- A wide (CJK) character counts two columns for wrapping; combining marks count zero.
- A panic or error exit still restores the terminal (alternate screen, raw mode, paste, keyboard mode).
- A very tiny terminal (height < 4) drops facts, then state rows; height 0 paints nothing.
- Model-supplied tool descriptions are stripped of control characters and OSC introducers and capped at 120 characters.

## Requirements *(mandatory)*

### Functional Requirements

**Screen and lifecycle**

- **FR-001**: The system MUST take the alternate screen, raw mode, bracketed paste and the kitty keyboard disambiguation flag on entry, and release all of them on clean exit, error return and panic, once.
- **FR-002**: The system MUST lay out, top to bottom: transcript (remaining rows), a 1-row state line (when height ≥ 3), an editor of 1–12 rows (capped at height − 2), and a 1-row facts line (when height ≥ 4).
- **FR-003**: The system MUST NOT write finished turns into the terminal's scrollback and MUST NOT capture the mouse.
- **FR-004**: The system MUST rebuild all transcript rows from their source entries on resize, preserving the reader's fractional scroll position, and MUST coalesce queued session events into one paint (minimum ~33 ms between bursts).
- **FR-005**: The system MUST print `resume: kelir tui --resume <id>` on leaving when a session id is known.
- **FR-006**: The spinner MUST tick every 100 ms only while a phase is working; idle sets no timer.

**Keys**

- **FR-007**: Enter MUST send; Alt-Enter, Shift-Enter and Ctrl-J MUST insert a newline.
- **FR-008**: Esc MUST send `stop` only while working, printing `» stop (Esc)`.
- **FR-009**: Ctrl-C MUST send `stop` the first time and `end` if pressed again within 1000 ms.
- **FR-010**: Ctrl-O MUST toggle an overlay showing the last tool call's head and the last 2048 bytes of its output; the facts row MUST read `tail (Ctrl-O closes) · …` while open.
- **FR-011**: Ctrl-L MUST redraw; Ctrl-K cut to end of line; Ctrl-U clear the whole buffer; Ctrl-D, Ctrl-Delete, Alt-Backspace and Alt-d delete the word after the cursor; Ctrl-Backspace (and Ctrl-H) delete the word before; Alt/Ctrl-←/→ and Alt-h/Alt-l move by word.
- **FR-012**: PageUp/PageDown MUST scroll one viewport; Home/End MUST jump to the top/bottom of the transcript (End resumes following).
- **FR-013**: Ctrl-Y MUST copy the last non-empty assistant markdown via an OSC 52 sequence and set note `last answer copied`; Ctrl-Shift-Y MUST write the whole plain-text transcript to `transcript.txt` in the session directory and set note `transcript saved: <path>`.
- **FR-014**: Up/Down MUST move between visual rows and, past the first/last row, browse submitted history, restoring the unsent draft past the newest entry; consecutive duplicates MUST NOT be added to history.
- **FR-015**: Key release events MUST be ignored.

**Slash commands**

- **FR-016**: A line not starting with `/` MUST be a `say`. A line starting with `/` MUST be parsed as a command; names are `stop`, `say-now` (`say_now`, `now`), `later`, `model`, `tools`, `compact`, `expand`, `quit` (`exit`), `help` (`?`).
- **FR-017**: Each session-changing command MUST be dispatched as the same verb `kelir ctl` sends (`stop`, `say` with `now`, `later`, `model`, `tools`, `compact`, `end`), tagged `from: tui`, with a local `» …` echo line before the answer.
- **FR-018**: `/help` MUST list every command with a one-line description and send no verb. `/expand` MUST be a local view with no verb or log event.
- **FR-019**: Verb replies MUST be shown as `» <verb>: <applies>` except for `say`, `later` and a `stop` that was `dropped`, which are silent; a failed reply MUST show `kelir: <error>` and set a note in the state row.

**State and facts rows**

- **FR-020**: The state row MUST show one of: idle (`● idle · ready`), thinking, writing, running (single tool shows its label; several show a count), rate limited, paused (`⏸ … · say continue`), error (`✗ … · say to retry`); elapsed time uses the forms `12s`, `1m05s`, `1h02m` and appears only while working.
- **FR-021**: A turn ending `paused`, `stopped` or `error` MUST append one dim outcome line (`<glyph> N step(s) · X tok`); a `done` turn MUST NOT. The outcome line MUST be removed when the next step starts.
- **FR-022**: The facts row MUST show, joined by ` · `: the session title (if any), the model (if known), and `<k> tok in` where the count is the prompt tokens of the last request, shortened to `N.Nk` from 1000.
- **FR-023**: While a provider or slot wait is active the facts row MUST show `kelir <phase> · <wait line>` with ` · N s left` for waits under six hours, and the wait line's `say stop to cancel` text MUST be rewritten to `esc stops` in the pane.
- **FR-024**: A `session_title` event MUST update the facts row and emit a terminal-title record; phase changes MUST emit the Herdr status word (working/blocked/error/done/idle).

**Transcript**

- **FR-025**: Prompt echoes, tool-line runs, prose and outcome lines MUST be separated by exactly one blank row. Echoes of the TUI's own prompts MUST NOT be duplicated when the matching `user` log event arrives; prompts from other sources MUST be shown.
- **FR-026**: Each settled tool call MUST be one line: head left, verdict right (head clipped first); failures red with one dim tail line (the last output line); cancelled calls show `⏹ cancelled` and are not failures; clean calls show only size (`B`, `KB`, `MB`).
- **FR-027**: Calls MUST be numbered in settle order for `/expand n`; `/expand` output MUST be capped at 40 wrapped rows with a count of omitted rows.
- **FR-028**: When the viewport is scrolled away, new rows MUST NOT move it and MUST increment a pending count shown as `<state> · N new lines · End follows`.
- **FR-029**: Errors (log or rejected verbs) MUST appear as red `kelir: <message>` lines; notes as plain lines; `source` events as `· <kind> <name>: <detail>`; verb events as `» <name> (<from>) · <boundary>` except `status` and `observe`; session end as `session ended: <reason>`.

**Markdown and wrapping**

- **FR-030**: Assistant text MUST render headings, bold, italic, strikethrough, inline code, fenced code (gutter, clipped), nested/ordered lists, task boxes, quote bars on every wrapped row, tables, links (`text (url)` when different), rules, images as `[image: alt]`, and raw HTML as literal text.
- **FR-031**: Streaming MUST commit finished fence lines and finished top-level list items while keeping three lines live, and MUST commit paragraphs and tables only when their block closes; committed rows MUST never vanish or be re-rendered per delta.
- **FR-032**: Wrapping MUST count display columns (wide = 2), break at spaces when possible, drop trailing spaces, keep leading ones, and hard-break words longer than the width.
- **FR-033**: Tables MUST be boxed at width ≥ 30 when the content fits, else rendered as `a | b | c` rows.
- **FR-034**: `NO_COLOR` (non-empty) MUST drop colour only; `KELIR_ASCII` (non-empty) or `TERM=dumb` MUST switch spinner, state glyphs, bullets, gutter, quote bar and table borders to ASCII.

**Images**

- **FR-035**: A bracketed paste of a path, or typed text ending in a path, that names a real image (png, jpg/jpeg, gif, webp, by file signature rather than extension) MUST become a chip `[image N]` and the file MUST be saved under `<session>/attachments/`; at most 8 chips per prompt.
- **FR-036**: On submit each chip label MUST be replaced by its pointer, and quoted/bare image paths in the text MUST be absorbed into pointers once each.
- **FR-037**: Removing an unsent chip MUST delete its saved file; a chip already sent MUST keep it.

**Tool descriptions**

- **FR-038**: Tool-call labels MUST use the call's `description` when present (control characters and OSC introducers removed, whitespace folded, ≤ 120 characters with `…`), otherwise `$ <command>`, `edit <path>` etc.

### Key Entities

- **Transcript entry**: a source unit (plain, dim, user prompt, error, markdown, settled markdown rows, call line) from which rendered rows are rebuilt at any width.
- **Call line**: ordinal, head, outcome, exit code, byte size, output path, tail line.
- **Action**: a parsed user intent (say, say-now, later, stop, model, tools, compact, quit) mapped to a verb and args.
- **Editor buffer**: lines, cursor, history, draft, chips.
- **Chip**: sequence number, saved relative path, pointer text, label `[image N]`.
- **Activity state**: kind (idle, thinking, writing, running, rate limited, paused, error), start time, detail, tool count.
- **Status/facts**: provider, model, tokens, title, phase, active wait.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Every slash command in the command table parses to the documented verb and arguments, and every malformed form yields a one-line message, verified by unit tests.
- **SC-002**: After a resize at any width ≥ 1, no transcript row is wider than the terminal and no entry is lost.
- **SC-003**: A 60-line streamed code block has ≥ 55 lines committed to the transcript before its closing fence arrives.
- **SC-004**: Any exit path (normal, `/quit`, double Ctrl-C, panic) leaves the terminal out of raw mode and the alternate screen.
- **SC-005**: A full session can be read, from first prompt to last answer, using only in-app scrolling.
- **SC-006**: Glyph state (idle, paused, error, working) remains distinguishable under `NO_COLOR` and `KELIR_ASCII=1`.

## Assumptions

- The TUI is started only when stdout is a terminal; non-interactive modes are out of scope.
- The banner text (provider, model, session, cwd, help line) is composed by the caller and passed in; this spec covers how it is displayed (as ordinary transcript lines, dim where flagged).
- Verb semantics (`say`, `stop`, `later`, `model`, `tools`, `compact`, `end`) and provider retry behaviour are owned by other features; this spec covers only how the TUI sends and shows them.
- The Herdr status protocol and the `kelir view` command are owned elsewhere.
- Colours are ANSI-16 and terminal-palette dependent.

## Code vs docs discrepancies

1. `docs/reference/tui.md:9-17` points to `src/tui.rs`, `src/theme.rs`, `src/activity.rs`, `src/markdown.rs`, `src/tools_view.rs`, `src/attachments.rs`, `src/commands.rs`, `src/editor.rs`; the code lives under `src/tui/…` (`src/tui/mod.rs:1-15`) and attachments in `src/features/attachments.rs`.
2. `docs/reference/tui.md` (Keys table) says Esc/Ctrl-O closes the tail overlay; only Ctrl-O closes it, Esc is stop-only-while-working (`src/tui/keys.rs:63`, `src/tui/app/input.rs:30`, `src/tui/app/input.rs:249`).
3. `docs/reference/tui.md` (Keys table) lists Ctrl-U twice (as part of "Ctrl-K / Ctrl-U" and as "clear the editor") and omits Alt-d (delete word after) and Alt-h/Alt-l (word moves) which are bound (`src/tui/keys.rs:56-58,66-70`).
4. `docs/reference/tui.md` (areas table) shows facts as `model · 1.5k tok`; the code shows `<k> tok in` (last request's prompt tokens, not a total) (`src/tui/view.rs:66-77`).
5. `docs/reference/tui.md` (banner section) says the facts-row wait reads `kelir working · …`; code uses the phase word of the status (`working`, `paused`, `error`; `done` reads `idle`) (`src/tui/view.rs:45-65`).
6. `docs/reference/tui.md` (Keys table) says ↑/↓ are history on first/last line; the code first moves by visual (wrapped) row and only then falls back to history (`src/tui/editor.rs:544-557`).
7. `docs/reference/tui.md` (Images) does not state the 8-images-per-prompt cap (`src/features/attachments.rs:19`, `src/tui/app/input.rs:88-91`).
8. `docs/reference/tui.md` (Keys table) says Home/End scroll the transcript; the editor's own Home/End handling (`src/tui/editor.rs:558-583`) is unreachable from the keymap (`src/tui/keys.rs:84-85`), so there is no key for start/end of the input line.

## Source map

- src/tui/mod.rs
- src/tui/app/mod.rs
- src/tui/app/input.rs
- src/tui/app/entry.rs
- src/tui/app/events.rs
- src/tui/app/layout.rs
- src/tui/view.rs
- src/tui/keys.rs
- src/tui/terminal.rs
- src/tui/commands.rs
- src/tui/editor.rs
- src/tui/editor_impl.rs
- src/tui/scrollback.rs
- src/tui/activity.rs
- src/tui/tools_view.rs
- src/tui/theme.rs
- src/tui/markdown/mod.rs
- src/tui/markdown/render.rs
- src/tui/markdown/stream.rs
- src/tui/markdown/table.rs
- src/tui/markdown/wrap.rs
- src/features/attachments.rs
- docs/reference/tui.md
- tests/it/tui.rs
- tests/it/tui_markdown.rs
- tests/it/tui_scroll.rs
- tests/it/tui_scrollback.rs
- tests/it/tui_status.rs
- tests/it/tui_images.rs
