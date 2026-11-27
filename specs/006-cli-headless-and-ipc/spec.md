# Feature Specification: CLI, Headless Runs, Session IPC and Notifications

**Feature Branch**: `006-cli-headless-and-ipc`

**Created**: 2026-11-27

**Status**: Reverse-engineered from main e1cd8ea

**Input**: Reverse-built from the code: the command surface (`src/main.rs`, `src/main_cli.rs`), the session socket and its clients (`src/ipc/`: ctl, attach, observe, tail, doctor), headless `kelir run` (including `--ask`), and event notifications (`src/features/notify.rs`).

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Run one task headlessly and get an answer (Priority: P1)

A script or a human runs `kelir run "<task>"` (or pipes the task on stdin) and the harness runs the task to completion without a terminal UI. With `--ask` only the final answer text goes to stdout; with `--json` the event log streams as NDJSON followed by a result record.

**Why this priority**: This is the scripting entry point. The agent, cron lines and other tools all depend on it.

**Independent Test**: Run `kelir run --ask "what is 2+2"` against a scripted model; stdout is exactly the final text and the exit code is 0.

**Acceptance Scenarios**:

1. **Given** a task passed as words, **When** `kelir run word word…` runs, **Then** the words are joined with single spaces into one task.
2. **Given** no task argument and piped stdin, **When** `kelir run` runs, **Then** the task is the trimmed stdin; empty input is a usage error (exit 2, "no task: pass it as an argument or on stdin").
3. **Given** `--ask`, **When** the turn ends with non-empty final text, **Then** that text is printed to stdout.
4. **Given** `--json`, **When** the run proceeds, **Then** each log event is written to stdout as one JSON line and a closing result record carries the outcome, exit code, final text, step count, session id and dir.
5. **Given** `--keep`, **When** the first turn ends, **Then** the process stays alive accepting `say`, `later` and `end` verbs.
6. **Given** `--resume <ID>` (an exact id, unique prefix or `latest`, dead or alive), **When** the run starts, **Then** the session continues from its log.
7. **Given** `--max-steps N`, **When** a turn reaches N steps, **Then** it pauses; `0` and unset both mean no cap.

---

### User Story 2 - Steer or query a running session with `kelir ctl` (Priority: P1)

An operator sends one verb (say, later, stop, model, tools, compact, skills, view, observe, cancel, status, brief, end) to a running session over its local socket and reads one JSON answer line.

**Why this priority**: It is the only way to influence a headless session and the base for every other IPC client.

**Independent Test**: Start `kelir run --keep`, then `kelir ctl <id> status --json`; one JSON line with `ok: true` is printed and the exit code is 0.

**Acceptance Scenarios**:

1. **Given** a running session, **When** `kelir ctl <id> say "text"` runs, **Then** the verb is sent with that session's token, one answer line is printed and the exit code is 0 when `ok` is true, 1 otherwise.
2. **Given** `status` without `--json`, **When** the session answers, **Then** a human-readable summary is printed; with `--json` the raw answer line is printed.
3. **Given** `brief`, **When** the session answers, **Then** the child's final text alone is printed (not JSON), and the client waits for a model-length period rather than the normal 10 s.
4. **Given** `observe`, **When** it is sent, **Then** it is answered from the session's log without touching the socket (US4).
5. **Given** an unknown verb or bad arguments, **When** validated locally, **Then** a message is printed and the exit code is 2 without connecting.
6. **Given** `latest` and the newest session is not running, **When** `ctl` runs, **Then** it reports that session as not running with exit 1 and never falls back to an older session.
7. **Given** a prefix matching several sessions, **When** `ctl` runs, **Then** candidates are tried newest first and the first one that answers receives the verb.

---

### User Story 3 - Watch a session from another terminal with `kelir attach` (Priority: P2)

A user replays a running or finished session's log through the same row model as the TUI, optionally following it live and, with `--steer`, sending `say` and `stop`.

**Why this priority**: It makes long headless runs observable without owning them.

**Independent Test**: Run `kelir attach <id> --plain` on a finished session; the transcript is printed and the exit code is 0.

**Acceptance Scenarios**:

1. **Given** a session id, unique prefix, `latest` or a name, **When** `attach` runs, **Then** the log is resolved and replayed; a name takes precedence over an id prefix.
2. **Given** `--since N`, **When** replaying, **Then** only the last N events are shown (`0` is all).
3. **Given** `--raw`, **When** replaying, **Then** the original NDJSON lines are printed unchanged; torn or malformed lines are skipped, never invented.
4. **Given** `--follow`, **When** new events are appended, **Then** they are rendered as they arrive until a `session_ended` event or until the owning process is gone (a crash), then a last drain and exit 0.
5. **Given** `--steer --follow`, **When** stdin lines `say <text>` or `stop` arrive, **Then** each is sent with the session's own token; any other verb is refused on stderr; `say` without text is refused; blank lines and `#` comments are ignored.
6. **Given** `--steer` without `--follow`, **When** invoked, **Then** it exits 2 with a clear message.
7. **Given** no flags besides the session, **When** attached, **Then** the viewer never writes to the session dir and opens no socket.

---

### User Story 4 - Inspect the next request and the key setup (`observe`, `doctor`) (Priority: P2)

A user prints exactly what the model will see next (`kelir observe`) and checks which credential each provider would use (`kelir doctor`) without exposing secrets.

**Why this priority**: These are the diagnosis tools when a run behaves oddly or cannot authenticate.

**Independent Test**: `kelir doctor --json` prints one JSON object with a provider list and an `ok` flag.

**Acceptance Scenarios**:

1. **Given** a session log, **When** `kelir observe <session>` runs, **Then** it prints the session, provider/model, step count, request URL, token estimate, system prompt, tools, messages, sources, hint, cache ratio and gates; `--raw` prints only the exact JSON request body.
2. **Given** a log with no `session_started`, **When** observed, **Then** it fails with a message.
3. **Given** `kelir doctor`, **When** run, **Then** each provider's base URL, key source and expiry are shown, never the key.
4. **Given** the session's own provider (or any expired credential) has no usable key, **When** `doctor` ends, **Then** the exit code is 1; a missing key on a non-default provider does not fail it.
5. **Given** `--provider` is not one of the two known providers, **When** `doctor` runs, **Then** exit 2.

---

### User Story 5 - Be notified when work ends (Priority: P3)

A user configures `[notify]` so a shell hook runs and/or a terminal record is emitted when a turn finishes, errors, pauses, or a background job or child settles.

**Why this priority**: Convenience for long runs; nothing is on by default.

**Independent Test**: Configure `turn_done = "<hook>"`, run a task; the hook runs with `KELIR_NOTIFY_EVENT=turn_done` and a `notify` note appears in the log.

**Acceptance Scenarios**:

1. **Given** no `[notify]` config, **When** any event occurs, **Then** nothing runs and nothing is logged.
2. **Given** hooks for `error`, **When** a turn errors, **Then** each hook runs in order through the configured shell with a scrubbed environment plus `KELIR_NOTIFY_EVENT` and `KELIR_NOTIFY_MESSAGE`.
3. **Given** a hook that fails or hangs, **When** it ends or hits 30 s, **Then** the failure is recorded and the tree is killed, but the turn and exit code are unchanged.
4. **Given** `terminal = true` on a TTY, **When** an event fires, **Then** an OSC 9 (default) or OSC 777 record is written with control characters folded to spaces.
5. **Given** `rate_limit = N`, **When** the same kind fires twice within N seconds in the same session, **Then** the second is skipped and logged as rate limited.

---

### User Story 6 - Child CLIs and housekeeping commands (Priority: P3)

Inside a session's bash call the agent uses `kelir view`, `kelir bg`, `kelir brief`, `kelir run --child`, `kelir ls`, `kelir kill`, `kelir mcp`; anywhere, `kelir docs` reads the embedded reference.

**Why this priority**: These are thin clients of other subsystems; this spec covers only their command-line contract.

**Independent Test**: `kelir docs --list` prints topics; `kelir view x.png` outside a session exits 1.

**Acceptance Scenarios**:

1. **Given** no `KELIR_SOCK`, **When** `kelir view <path>` runs, **Then** it exits 1 saying it must run from a kelir tool call.
2. **Given** `KELIR_SOCK` and a valid image, **When** `view` succeeds, **Then** it prints `image queued: appears in your next message` and exits 0; a refusal prints the session's error and exits 1.
3. **Given** `run --child` outside a session, **When** invoked, **Then** it behaves as an ordinary run; `--child --background` outside a session exits 2.
4. **Given** `kelir docs`, **When** run with no topic, `--list`, `--search` or `--skill`, **Then** the embedded reference is printed (index, topics, hits or a builtin skill).

---

### Edge Cases

- A bare `kelir` (no subcommand) or `kelir tui` without a TTY on stdout exits 2 with advice to use `run` or `run --ask`.
- Flags before the subcommand (`kelir --model x tui`) fill in what `tui` did not set.
- Socket requests: a line over 64 KiB or a connection silent for 10 s is refused once and closed; a request with a bad token gets one error and the connection closes; an unparseable line gets an error but the connection stays open.
- Session socket path too long for a Unix socket: the session binds a short fallback path and records its location so clients still find it; if even that is too long the error names `KELIR_STATE_DIR`.
- A client that disconnects while its verb is pending tells the session nobody is waiting.
- Attach: a log line over 1 MiB is skipped, a torn last line is held until completed, a replaced log starts over; the viewer keeps at most 2000 rendered rows.
- `attach` on a session without `events.jsonl`, on an ambiguous prefix, or on no match exits 2.
- `run --task-file` conflicts with positional task words; `--background` requires `--child`; `--name` conflicts with `--resume` and `--child`; `--single-flight`, `--fresh` require `--name`; `--timeout` requires `--single-flight`.
- A hook with no configured shell reports "no shell configured".
- A one-shot process waits up to 5 s for outstanding hooks before exiting.
- `kelir ctl <s> observe` accepts `raw` or `--raw` as an argument.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: Running `kelir` with no subcommand MUST start the interactive session, with the same flags as `kelir tui`, and MUST exit 2 when stdout is not a terminal.
- **FR-002**: The CLI MUST expose these subcommands: `run`, `tui`, `attach`, `ctl`, `view`, `bg`, `observe`, `mcp`, `brief`, `ls` (with `sessions`, `rename`, `forget`), `kill`, `channel`, `doctor`, `docs`, plus hidden internal `bg-shim`, `gate-guard`, `child-host`.
- **FR-003**: Session-running commands (`run`, `tui`, `channel`, `doctor`) MUST accept the common flags `--model`, `--provider`, `--cwd`, `--config`, `--max-steps`.
- **FR-004**: A usage error (bad flag, bad config, unknown provider, empty task) MUST exit 2; a refusal or failed run MUST exit 1; success MUST exit 0. Scheduled runs additionally use 75 (skipped, lock held) and 124 (deadline).
- **FR-005**: `run` MUST take its task from positional words (joined by spaces), else from `--task-file`, else from stdin (trimmed, non-empty).
- **FR-006**: `run --ask` MUST print the final text to stdout when it is non-empty; `run --json` MUST stream log events as NDJSON and end with a result record.
- **FR-007**: `run --keep` MUST keep the session alive after a turn; `--resume` MUST accept an exact id, unique prefix or `latest`.
- **FR-008**: `run --name` MUST create or resume the named session; `--single-flight`, `--timeout` and `--fresh` MUST hand the run to the scheduled-run path (status file, lock, deadline).
- **FR-009**: `run --child` inside a session (`KELIR_SOCK` set) MUST delegate to the child-session client; outside one it MUST run normally (and `--background` there MUST exit 2).
- **FR-010**: The session MUST serve newline-delimited JSON requests (`verb`, `args`, `from`, `token`) on a local socket restricted to its owner, answering each with one JSON line (`ok`, `verb`, `seq`, `applies`, `state`, `error`).
- **FR-011**: The socket MUST reject unauthenticated requests (one error, then close), request lines over 64 KiB, and connections idle 10 s before sending a request.
- **FR-012**: Clients MUST wait at most 10 s for connect and answer, except `brief` and `mcp` which wait for model/tool duration.
- **FR-013**: `ctl` MUST validate the verb and arguments locally before connecting, read the session's token from its own directory, print one answer line (or the `status`/`brief` readable forms), and exit 0 on `ok`, 1 otherwise.
- **FR-014**: `ctl latest` MUST address only the newest session and fail if it is not running; an id prefix MUST try matching sessions newest first.
- **FR-015**: `ctl` MUST answer `observe` from the log without a socket request.
- **FR-016**: `attach` MUST resolve a name first, then an exact id, unique prefix or `latest`; an ambiguous prefix or no match MUST exit 2.
- **FR-017**: `attach` MUST feed log events to the same row model as the TUI, support `--since`, `--raw` (raw NDJSON), `--plain`, `--follow`, and stay read-only (no writes, no socket) without `--steer`.
- **FR-018**: `attach --follow` MUST stop after `session_ended` or when the session's owner process is gone, and MUST read bounded pages with a bounded 2000-row backlog.
- **FR-019**: `attach --steer` MUST require `--follow` (else exit 2) and send only `say <text>` and `stop`, with the session's token, from stdin lines.
- **FR-020**: The tail reader MUST never load the log whole, MUST hold a partial last line until complete, MUST skip lines over 1 MiB, and MUST restart when the file is replaced.
- **FR-021**: `observe` MUST rebuild the next request from the log with the same fold and request builder as the live loop, print a human report or (`--raw`) just the JSON body, and exit 2 on a bad config, 1 on a missing session or log.
- **FR-022**: `doctor` MUST report, for each known provider (or the one named), base URL, key source, expiry and read time without ever printing the key, in text or one JSON object, and MUST exit 1 only when the session's provider fails or a credential is expired.
- **FR-023**: `view` MUST send the `view` verb over `KELIR_SOCK`, exit 1 outside a session, print a fixed confirmation on success and the session's refusal on failure.
- **FR-024**: Notifications MUST be fired only for `turn_done`, `error`, `paused` (turn end), `bg_settled` and `child_settled`, and MUST do nothing unless a hook or `terminal = true` is configured.
- **FR-025**: Configuring a hook key outside these five kinds, a non-string command, or a `terminal_kind` other than `osc9`/`osc777` MUST be a config error.
- **FR-026**: Hooks MUST run in order through the configured shell, in a scrubbed environment with `KELIR_NOTIFY_EVENT` and `KELIR_NOTIFY_MESSAGE`, with no stdin, a 30 s limit (overridable only for tests), the whole process tree killed, never blocking the session.
- **FR-027**: A hook failure MUST NOT alter the turn, tool results or exit code; a dispatch MUST be recorded as a `notify` note reading `<kind>: <N> hook(s) fired` or `<kind>: <error>`.
- **FR-028**: Rate limiting MUST be per session directory and per kind, using `rate_limit` seconds (default 0).
- **FR-029**: A terminal record MUST be written only when stdout is a TTY; its message is `<title or "kelir">: <kind>[ · <detail, 120 chars>]` with control characters replaced by spaces.
- **FR-030**: At process exit a one-shot run MUST give outstanding hooks up to 5 s to finish.
- **FR-031**: `docs` MUST print the embedded reference (index, topic, `--list`, `--search`, `--skill`) and exit 1 for an unknown topic or a search with no hit.

### Key Entities

- **Session**: an event-logged run with an id, optional name, directory (`events.jsonl`, `token`, socket), and an owner process.
- **Verb request / answer**: one JSON line each way over the session socket, carrying the session token.
- **Event log (`events.jsonl`)**: the append-only record that attach, observe and resume read.
- **Notify config**: hooks per event kind, terminal flag and kind, rate limit, title.
- **Notify note**: a `note` event of kind `notify` recording a dispatch outcome.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: `kelir run --ask` returns only the final text, with exit 0 on success and a nonzero exit on error, in every scripted test.
- **SC-002**: A verb sent with `ctl` is answered with one JSON line within 10 s for all verbs except `brief` and `mcp` calls.
- **SC-003**: An unauthenticated or oversized socket request never reaches the session loop.
- **SC-004**: `attach` on a 100,000-event log holds at most 2000 rendered rows and never reads the whole file at once.
- **SC-005**: `attach` without `--steer` leaves the session directory byte-identical.
- **SC-006**: A failing or hanging notify hook never changes a run's exit code and ends within 30 s.
- **SC-007**: `doctor` output never contains a credential value.
- **SC-008**: Every documented exit code (0, 1, 2, 75, 124) is reproduced by at least one test.

## Assumptions

- Local single-user machine; the socket's permissions (0600 on Unix, owner-only pipe ACL on Windows) are the access boundary alongside the token.
- Windows and Unix behave alike; the socket is a Unix socket or a named pipe.
- The scheduled-run internals (names, STATUS, lock) belong to another spec; here only the flags that route to them are covered.
- Terminal UI rendering rules belong to the TUI spec; `attach` only reuses them.

## Code vs docs discrepancies

1. `docs/reference/attach.md:28` says a 4,000-row backlog; `src/ipc/attach.rs:17` is `BACKLOG_ROWS = 2000`.
2. `docs/reference/attach.md:10`, `docs/reference/notify.md:9`, `docs/reference/cli.md:7` cite `src/attach.rs`, `src/tail.rs`, `src/notify.rs`, `src/cli.rs`; the code is in `src/ipc/attach.rs`, `src/ipc/tail.rs`, `src/features/notify.rs`, `src/main_cli.rs`.
3. `docs/reference/cli.md` (`ctl` table) says the session may be "a unique prefix"; `src/ipc/ctl.rs:48` accepts an ambiguous prefix and tries each match newest first instead of rejecting it.
4. `docs/reference/notify.md` says `kelir run --json` never emits a terminal record; `src/features/notify.rs:256` gates only on stdout being a TTY, and the notify path has no `--json` check, so `--json` on a TTY would emit one.
5. `docs/reference/notify.md` says the config examples cover "bg job settled"/"child settled" with the same keys, but its example block shows neither `bg_settled` nor `child_settled`; the kinds are `src/features/notify.rs:142-148`.
6. `docs/reference/attach.md:18,40` use milestone tags ("M9 name", "Children (M7)"), which AGENTS.md forbids in code comments and which carry no meaning in the behaviour.
7. `docs/reference/cli.md` intro lists `run`, `ctl`, `view`, `bg`, `ls`, `doctor`, `docs` only; `src/main_cli.rs:24-62` also has `attach`, `observe`, `mcp`, `brief`, `kill`, `channel`, `tui`.

## Source map

- src/main.rs
- src/main_cli.rs
- src/ipc/mod.rs
- src/ipc/ctl.rs
- src/ipc/attach.rs
- src/ipc/tail.rs
- src/ipc/observe.rs
- src/ipc/doctor.rs
- src/core/session.rs
- src/core/session/boot.rs
- src/core/session/report.rs
- src/core/session/view.rs
- src/core/session/util.rs
- src/features/notify.rs
- src/features/names/namedrun.rs
- src/features/children/cli.rs
- src/config/resolve.rs
- src/paths.rs
- docs/reference/cli.md
- docs/reference/attach.md
- docs/reference/notify.md
