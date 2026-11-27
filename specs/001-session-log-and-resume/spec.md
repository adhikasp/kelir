# Feature Specification: Session log, resume, verbs and compaction

**Feature Branch**: `001-session-log-and-resume`

**Created**: 2026-11-27

**Status**: Reverse-engineered from main e1cd8ea

**Input**: User description: "Reverse-build the spec for kelir's session lifecycle: the append-only JSONL event log and its durability, resuming a session from that log, `kelir ls`, compaction, the boundary between core and features, and the verbs a running session accepts."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - The log is the only truth and survives a crash (Priority: P1)

Every session writes one append-only file of JSON events. Context sent to the model is always rebuilt from that file, so a session killed at any instant can be picked up with nothing lost except the step that was in flight.

**Why this priority**: Everything else (resume, `ls`, compaction, gates, the TUI) reads this log. If it is not durable and unambiguous, nothing else can be trusted.

**Independent Test**: Start a session against a mock provider, kill the process mid-write, then read the log: every line parses, `seq` is gapless from 1, and the torn fragment is preserved beside the log.

**Acceptance Scenarios**:

1. **Given** a new session, **When** it starts, **Then** the first event is `session_started` with `format: 1`, carrying the frozen system prompt, tool list, provider, model and cwd, and `seq` counts up from 1 with an RFC 3339 `ts` on each line.
2. **Given** a log whose last line was cut off mid-write, **When** the session is reopened, **Then** the fragment is moved to `events.jsonl.torn` (one record per tear, stamped), the log holds whole events only, and the next event continues the sequence without reusing a `seq`.
3. **Given** a last line that is a complete event but lacks its trailing newline, **When** it is read, **Then** the event is kept (a newline is appended on reopen), not discarded.
4. **Given** a log whose first event is not `session_started` with `format: 1` (or is unparseable), **When** anything reads it, **Then** it is refused with "written by a different kelir format; start a new session" and no migration is attempted.
5. **Given** any string payload over 10 KB, **When** its event is written, **Then** it is stored under `blobs/<seq>-<field>` and the event holds a `{spilled, path, bytes, preview}` reference, while the request built from the log is byte-identical to one built from inline values.
6. **Given** a session in progress, **When** a client's verb is accepted, **Then** the `verb` event is on disk (fsync'd) before the `ok` answer is sent.

---

### User Story 2 - Resume a stopped, crashed or paused session (Priority: P1)

A user runs `kelir run --resume <id|prefix|latest> ["<task>"]` (or the TUI with `--resume`) and the session continues with its original system prompt, model, tools and cwd, never repeating a tool call that may have already run.

**Why this priority**: Surviving crashes, provider outages and restarts is the stated purpose of the log.

**Independent Test**: Kill a session while a `bash` call is running, resume it, and assert on the log that the call is settled `unknown` exactly once and was not executed again.

**Acceptance Scenarios**:

1. **Given** a log with a tool call the model requested or that started but never settled, **When** the session is resumed, **Then** exactly one `call_settled` with outcome `unknown` is written per such call, whose result says the outcome is unknown and to check before retrying (with the partial-output size and path when output had begun), and the call is not re-run.
2. **Given** an MCP call that started but never settled, **When** resumed, **Then** it is settled `unknown` too.
3. **Given** a log damaged by a torn last line or unparseable earlier lines, **When** resumed, **Then** an `error` event with location `log` reports each damage once; a later resume does not report the same message again.
4. **Given** the last turn ended `stopped`, `paused` or `error`, or the log was never closed with `session_ended`, or a turn had begun with no `turn_ended`, **When** resumed with no new task, **Then** the turn is continued; if the last context message is the model's own answer, a user message "[kelir] the session was interrupted between turns; continue the task from where it stopped." is added first.
5. **Given** a finished session (last turn `done`, log closed) and no new task and not `--keep`, **When** resumed, **Then** the log gets `session_ended` with reason `resumed` and the process exits 0 without a turn.
6. **Given** a resume with a task, **When** it starts, **Then** the task is added as a new message on the existing log (delivery `next` if a turn was in flight, otherwise `now`).
7. **Given** a session whose `model` verb changed provider or model, **When** resumed, **Then** the latest `step_started` provider/model is used, while id, cwd, system prompt and tool list come from the log and config from today.
8. **Given** a session another process is running, **When** a resume is attempted, **Then** it fails with "already running" unless that session's log is already closed, in which case it waits up to 2 s for the old socket to go away.
9. **Given** the id is a unique prefix, `latest`, or a full id, **When** a command takes a session, **Then** it resolves; an unknown or ambiguous prefix is an error that says so.
10. **Given** background jobs that settled while nothing was running, **When** resumed, **Then** their status is folded in and the resumed session treats it as work to do.
11. **Given** a resumed one-shot run whose turn fails with a retryable provider error, **When** it fails, **Then** the turn is retried a bounded number of times before the process exits 1.

---

### User Story 3 - Steer a running session with verbs (Priority: P1)

A human, script or gate sends a verb (`kelir ctl <session> <verb> …`, a TUI slash command, or a job) over the session socket. Each verb is logged first, then applied at exactly one named boundary, never between a tool call and its result.

**Why this priority**: This is the only way to steer, inspect or end a live session.

**Independent Test**: Send each verb to a session driven by a scripted provider and assert on the `verb` event (name, args, from, boundary) and the answer envelope.

**Acceptance Scenarios**:

1. **Given** a running session, **When** `say "text"` is sent, **Then** a `verb` event with boundary `queued` is logged, the answer says so, and at the next boundary a `user` event with delivery `next` is written.
2. **Given** a running tool call, **When** `say --now` is sent, **Then** the running calls are stopped first, and the `user` event has delivery `now`.
3. **Given** a running turn, **When** `later "text"` is sent, **Then** it applies only at turn end and is delivered then.
4. **Given** running calls, **When** `stop` is sent, **Then** every running call settles `stopped` and the turn ends `stopped`; `stop --call <id>` for a live call stops only that call and the turn continues; an unknown call id is refused with "no running call".
5. **Given** a session, **When** `model <provider>/<model>` or `<model>` is sent, **Then** it applies before the next provider request and the next `step_started` carries it; a model that cannot be swapped is recorded as an `error` event (location `verb`) and the old one keeps working.
6. **Given** `tools --disable X` / `--enable X`, **When** applied, **Then** the set of disabled tools changes for later calls; an unknown tool name is refused at parse time (MCP names `mcp:<server>` and `mcp:<server>.<tool>` are accepted).
7. **Given** `skills --name N (--path P | --text T) [--kind skill|instruction]`, **When** applied, **Then** a `source` message arrives at the tail; the frozen system prompt is never rewritten. A name outside letters, digits, `.`, `_`, `-` is refused.
8. **Given** `status`, **When** sent, **Then** it answers at once with a read-only snapshot, is never logged, and works even mid-request or while waiting to retry.
9. **Given** `end`, **When** sent, **Then** running calls are stopped, the turn ends and the log is closed with `session_ended` reason `end`.
10. **Given** a malformed or unknown verb, a bad `view` path, or a foreign background-job id for `cancel`, **When** received, **Then** a `verb` event with boundary `rejected` and an `error` is logged (within a bounded refusal budget), the answer is `ok: false` with the reason, and nothing else happens.
11. **Given** `kelir ctl` with a bad invocation, **When** run, **Then** it is refused client-side by the same parser before anything is sent.
12. **Given** a session that is not running, **When** `kelir ctl` targets it, **Then** it exits 1 (a message to a stopped session is a resume).

---

### User Story 4 - Compact a long context (Priority: P2)

When the context grows too large, an earlier range is replaced by a model-written summary. A human (`compact [focus]`), the model (the `compact` tool) or an automatic threshold can trigger it. Whatever the trigger, it applies just before the next provider request.

**Why this priority**: Without it long sessions hit the window; but short sessions work without it.

**Independent Test**: Feed the fold a log with N messages and a `compacted` event and assert the request keeps the system prompt, the summary, pinned human messages and the last few messages.

**Acceptance Scenarios**:

1. **Given** enough history, **When** compaction runs, **Then** a `compacted` event records `through_seq`, `summary`, `pinned_seqs`, `by` (`user`, `agent` or `auto`), `tokens_before`, `tokens_after` and `cache_restart: true`, and the summarising call's usage is a `step_usage` event with `kind: "compact"`.
2. **Given** a compaction, **When** the context is rebuilt, **Then** the first user message and every message typed by a human (from `tui`, `ctl`, `cli` or `channel`) up to the cut stay verbatim, the most recent messages (default 4) stay, and a tool result is never separated from the call that made it.
3. **Given** the context passes `min(window × 0.8, 150 000 tokens)` (defaults; both configurable), **When** the next step begins, **Then** an automatic compaction is requested; with no known window a 128 000-token default applies.
4. **Given** an automatic compaction whose result would not be smaller, **When** it is evaluated, **Then** it is skipped, one `error` event (location `compact`) is written for the episode, and it is not retried until the context grows past the size at the skip.
5. **Given** a user-requested compaction that does not shrink the context, **When** it runs, **Then** it still applies and the event records both numbers.
6. **Given** a summary request that fails or returns no text, **When** it fails, **Then** an `error` event (location `compact`, retryable) is written, the context is unchanged and the turn continues.
7. **Given** a second compaction, **When** it runs, **Then** it covers the earlier one (summarising its summary) and the earlier one stops rendering.
8. **Given** a session killed after the `compact` tool reported "compaction queued" but before it applied, **When** resumed, **Then** the compaction is queued again.
9. **Given** nothing old enough to summarise, **When** `compact` is sent, **Then** it reports so and writes no event.

---

### User Story 5 - Find sessions with `kelir ls` (Priority: P3)

`kelir ls` shows the tree of sessions (a child names its parent in `session_started`); `kelir ls sessions` lists named and recent sessions with state, model and activity; `rename` and `forget` manage names without touching logs.

**Why this priority**: Navigation aid; the log works without it.

**Independent Test**: Create a parent with a child session on disk and assert the tree and `--json` output.

**Acceptance Scenarios**:

1. **Given** sessions on disk, **When** `kelir ls` runs outside a session, **Then** the newest 10 roots appear as a tree; `--all` shows every session; `--json` prints the tree as JSON.
2. **Given** a session with no `session_ended` and no listening socket, **When** listed, **Then** it is shown as not alive (crashed/stopped), not running.
3. **Given** `kelir ls sessions`, **When** run, **Then** named sessions always appear and unnamed ones are capped to the 20 newest; children are hidden unless `--all`.

---

### User Story 6 - Core stays small; features plug into the boundary (Priority: P3)

Features (MCP, gates, sources, models, children, …) reach the log only through the core's single writer; the core never names a feature.

**Why this priority**: Architecture rule; nothing user-visible breaks if it slips, but it is enforced by `scripts/shape.sh`.

**Independent Test**: `bash scripts/shape.sh` passes, and a feature's event shows up in the log in the same order as a core event.

**Acceptance Scenarios**:

1. **Given** a feature writes an event, **When** it does, **Then** it goes through the core writer, so `seq` stays single-sourced and oversized payloads spill the same way.
2. **Given** a feature needs to record a `user` event, **When** it tries, **Then** it cannot: only the core writes `user` events (and so only the core derives the session title).

---

### Edge Cases

- A session starts with a task whose first non-empty line is blank or whitespace: the title is `untitled`; titles are flattened to one line and cut at 60 characters with `…`.
- Two sessions resumed at once: the second is refused ("already running") rather than forking the sequence.
- `stop` sent to an idle session: dropped, and the answer says `dropped`, not `now`.
- A job that settles reports through the `bg` verb, but only from `from: job`; any other sender is refused.
- `cancel` of a job this session did not start is refused and recorded.
- `bg-spawn` is not logged as a normal verb (only its shape, never its content).
- Request lines with unknown fields are refused, not ignored (wire envelope is strict).
- `model` with whitespace in the name, or an empty string, is refused at parse time.
- A `say` dropped by a gate writes no `user` event.
- A `session_ended` reason of `resumed` is written on an already-finished log; the file can therefore hold several `session_ended` events.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST write each session's events to `<state>/sessions/<id>/events.jsonl` as one JSON object per line with `seq` (monotonic, from 1), `ts` and `type`.
- **FR-002**: The first event MUST be `session_started` with `format: 1`; a reader MUST refuse any log whose first event is not, with a fixed message and no migration.
- **FR-003**: The log MUST be append-only; each append is flushed to the OS, and fsync happens at batch boundaries and before a verb is acknowledged.
- **FR-004**: On reopen the system MUST cut a torn last line, keep it in `events.jsonl.torn`, keep a complete-but-unterminated final event, and continue `seq` after the last valid event.
- **FR-005**: A string payload over 10 KB MUST be spilled to `blobs/<seq>-<field>`, referenced by `{spilled, path, bytes, preview}`, and folded back byte-identically.
- **FR-006**: The model's context MUST be a pure function of the log (`fold`), identical live and on resume: frozen system message, then `user`, `assistant`, tool-call results and `source` events in `seq` order, with `compacted` events replacing their range.
- **FR-007**: The system MUST record these 16 event types: `session_started`, `user`, `source`, `step_started`, `assistant`, `step_usage`, `verb`, `call_started`, `call_settled`, `turn_ended`, `compacted`, `error`, `session_ended`, `session_title`, `note`, `gate`.
- **FR-008**: The system MUST write `session_title` once, from the first `user` event whose `from` is not `cli`, as its first non-empty line, whitespace-flattened, at most 60 characters, `untitled` if empty.
- **FR-009**: A turn MUST end with `turn_ended` whose outcome is `done`, `stopped`, `paused` (with reason `steps` or `provider`) or `error`; the session MUST end with `session_ended` carrying a reason (`done`, `stopped`, `paused`, `error`, `end`, `denied`, `resumed`).
- **FR-010**: A one-shot run MUST exit 0 for done/paused/stopped and 1 for an error or a task denied by a gate; a long-lived (`--keep`/TUI) session MUST survive a failed turn.
- **FR-011**: Resume MUST NOT re-run any tool or MCP call; each unsettled call MUST get exactly one `unknown` settlement, including partial-output size and path when known.
- **FR-012**: Resume MUST take id, cwd, system prompt, sources and tool list from the log, provider/model from the last `step_started`, and config from the current machine.
- **FR-013**: Resume MUST refuse a session that is running (a live socket) unless its log is closed, and MUST report each log-damage message once across resumes.
- **FR-014**: Resume MUST continue a turn when the last turn ended `stopped`, `paused` or `error`, when the log is unclosed, or when a turn is unfinished; it MUST insert the continuation note when the folded context would otherwise end on an assistant message.
- **FR-015**: Resume of a finished session with no task and no `--keep` MUST write `session_ended` (`resumed`) and exit 0.
- **FR-016**: Session specs for `--resume`, `ctl` and similar MUST resolve by full id, unique prefix or `latest`, and MUST error on no match or an ambiguous match.
- **FR-017**: Every verb MUST be logged as a `verb` event (name, whitelisted args, from, boundary, optional error) before it is answered or applied, except `status` (never logged) and the internal `bg-spawn`/`bg-claim` (answered directly).
- **FR-018**: A verb's logged args MUST be limited to a per-verb whitelist; `bg-spawn` logs only its shape.
- **FR-019**: The system MUST accept the verbs `say`, `later`, `stop`, `model`, `tools`, `skills`, `compact`, `view`, `bg`, `cancel`, `brief`, `status`, `end`, validate arguments at parse time, and apply each at its boundary: `say` queued or now (`--now`), `later` turn end, `stop`/`end`/`cancel`/`bg`/`brief`/`status` at the first boundary, `model`/`tools`/`skills`/`compact` and `view` just before the next provider request.
- **FR-020**: A rejected verb MUST be recorded with boundary `rejected` and its error (bounded by a refusal budget) and answered `ok: false`; the session otherwise continues unchanged.
- **FR-021**: `bg` MUST be accepted only from `from: job`; `cancel` MUST be accepted only for jobs this session started.
- **FR-022**: The socket protocol MUST be one request line in and one answer line out, strict about unknown fields, and every verb except those handled before the token check MUST carry the session token.
- **FR-023**: A verb failing at apply time (`model`, `skills`, `view`, `cancel`) MUST write an `error` event and MUST NOT end the session.
- **FR-024**: Compaction MUST apply only at the boundary before a provider request, via the `compact` verb, the `compact` tool, or the auto trigger at `min(window × auto_at, soft target tokens)`.
- **FR-025**: Compaction MUST pin the first user message and every human-typed message within its range, keep the last N messages (default 4) verbatim, never split a tool result from its call, and never cut before an existing compaction's end.
- **FR-026**: The summarising request MUST be the live request up to the cut (same system message, `tools` and messages) followed by one user message with the prompt and focus, with `tool_choice: "none"` (dropped and remembered if the host rejects it); a reply with no text MUST be re-asked once; its usage MUST be logged as `step_usage` with `kind: "compact"`; the `compacted` event MUST record before/after token estimates and `cache_restart: true`.
- **FR-027**: An automatic compaction that would not shrink the context MUST be skipped and not re-requested until the context outgrows the size it was skipped at; a failed summary MUST leave the context unchanged.
- **FR-028**: Features MUST append events only through the core writer; the core MUST be the only writer of `user` events and session titles.
- **FR-029**: `kelir ls` MUST list sessions as a tree (children by `parent`), default to the 10 newest roots outside a session, support `--all` and `--json`, and mark sessions with no end and no live socket as not running; `ls sessions`, `rename` and `forget` MUST manage names without deleting logs.

### Key Entities

- **Session**: One process and one directory `<state>/sessions/<ULID>/`; its id is also the provider's prompt-cache key and never changes.
- **Event**: A logged fact `{seq, ts, type, …}`; the unit of truth.
- **Blob**: A payload spilled out of an event into `blobs/`.
- **Turn / Step**: A turn is one task from start to `turn_ended`; a step is one provider request within it.
- **Call**: A tool, MCP, background or child action with `call_started` and one `call_settled`.
- **Verb**: A request to a running session `{verb, args, from, token}` with a boundary and an answer envelope.
- **Boundary**: A named point where a verb applies: `now`, `queued`, `next_step`, `after_tools`, `after_step`, `turn_end`, or `rejected`.
- **Compaction**: A `compacted` event: a cut point, a summary and the pinned messages that stay verbatim.
- **Torn fragment**: Bytes of a cut-off last line kept in `events.jsonl.torn`.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: After killing a session at any point, 100% of reopens yield a log where every line parses and `seq` has no gap or duplicate.
- **SC-002**: After a resume, zero tool calls run twice: every previously unsettled call has exactly one `unknown` settlement and no second `call_started`.
- **SC-003**: A context built by `fold` after a resume equals the context built before the kill, byte for byte, for the part that survived.
- **SC-004**: Every accepted verb has a `verb` event on disk before its `ok` answer is observed by the client.
- **SC-005**: A `status` request is answered without writing any event, even during a provider request or retry wait.
- **SC-006**: A compaction event always shows `tokens_after` below `tokens_before` when triggered automatically; a non-shrinking automatic compaction never writes a `compacted` event.
- **SC-007**: A log written in any format other than 1 is refused 100% of the time with the fixed message.
- **SC-008**: `docs/reference/verbs.md`, `events.md` and `files.md` agree with the code on every verb and event name (checked by `cargo test --lib docs`).

## Assumptions

- Single-writer model: one process owns a session's log at a time; the live socket proves ownership.
- The state directory is local, with an OS that supports fsync and append.
- Providers may fail at any time; the log, not process memory, is how a session recovers.
- Token counts are estimates (4 bytes per token) unless the provider reports usage.
- `kelir ls` internals for named sessions (`names/`) belong to the names feature; only the visible listing is described here.
- Children, gates, MCP and background jobs are separate specs; here they appear only where they touch the log or verbs.

## Code vs docs discrepancies

1. `docs/reference/events.md:17` says format 1 has 16 event types, but its table (`:29`–`:47`) lists 15: the `error` event (`src/log/event/types.rs` `Error`; `src/log/event/log.rs:28` `NAMES`) is undocumented.
2. `docs/reference/events.md:6`, `:25`; `docs/reference/files.md:9`, `:97`, `:98`; `docs/reference/verbs.md:6`, `:89` name `src/event.rs`, `src/fold.rs`, `src/verbs.rs`, `src/session.rs` (`apply_verb`); the code is in `src/log/event/`, `src/log/fold.rs`, `src/core/verbs.rs`, and `apply_pending` in `src/core/session/verbloop.rs:283`.
3. `src/main_cli.rs:191` lists an `observe` verb in `kelir ctl --help`, but `src/core/verbs.rs:340` and `Verb::NAMES` (`:94`) have no such verb: it is rejected as unknown.
4. `docs/reference/events.md:47` says `session_title` is the first human prompt; `src/core/session/report.rs:94-97` skips any `user` event with `from == "cli"`, so a task given on the `kelir run` command line never titles the session until a later non-`cli` message arrives.
5. `docs/reference/verbs.md:48` gives `view` as applying `now`; `src/core/verbs.rs:100-130` (`scope` says `now`, `applies_at` only `BeforeStep`) means it lands at the next provider-request boundary.
6. `docs/reference/verbs.md:55-63` boundary table omits `after_step` (`src/core/verbs.rs:31`, applied at `src/core/session/turnloop.rs:236`); `after_tools` is listed as an answer value but no verb's `scope` returns it (`src/core/verbs.rs:96-110`).
7. The `applies` row of the answer envelope (`docs/reference/verbs.md:25`) lists `now`, `next_step`, `after_tools`, `turn_end`; the code also answers `queued` and `rejected` (`src/core/verbs.rs:96-101`, `src/core/session/verbloop.rs:44`) and `dropped` for a `stop` with nothing to stop (`verbloop.rs:11-14`); `dropped` is documented nowhere.

## Source map

- src/core/session.rs
- src/core/session/boot.rs
- src/core/session/lifecycle.rs
- src/core/session/mainloop.rs
- src/core/session/report.rs
- src/core/session/verbloop.rs
- src/core/session/compactloop.rs
- src/core/session/turnloop.rs
- src/core/compact.rs
- src/core/verbs.rs
- src/core/boundary.rs
- src/log/event/log.rs
- src/log/event/types.rs
- src/log/event/payload.rs
- src/log/event/mod.rs
- src/log/resume.rs
- src/log/fold.rs
- src/paths.rs
- src/main_cli.rs
- src/features/children/cli.rs
- src/features/names/mod.rs
- src/config/load.rs
- docs/reference/verbs.md
- docs/reference/events.md
- docs/reference/files.md
- AGENTS.md
