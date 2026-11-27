# Feature Specification: Background Jobs, Named Runs, Child Sessions and Herdr Status

**Feature Branch**: `007-background-jobs-and-subagents`

**Created**: 2026-11-27

**Status**: Reverse-engineered from main e1cd8ea

**Input**: User description: "Reverse-build the spec for kelir's background jobs (`kelir bg`, promoted bash calls), scheduled/named runs (`jobs.md`), child sessions and briefs (`children.md`, `kelir run --child`, `kelir brief`, `kelir ls`, `kelir kill`) and Herdr integration (status records, pane line)."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Run a long command without blocking the agent (Priority: P1)

An agent (or a human in a terminal) starts a long-running command as a background job, gets an id back at once, and later reads its output, waits for it or cancels it. When the job settles inside a session, the agent is woken with a message carrying the result.

**Why this priority**: Long builds and servers are the most common reason an agent loop would otherwise stall or lose work. Everything else here builds on the same status-file mechanism.

**Independent Test**: Run `kelir bg run sleep 1` (no session needed), then `kelir bg ls`, `kelir bg wait <id>`, `kelir bg out <id>`; verify the id, state transitions and output file.

**Acceptance Scenarios**:

1. **Given** a configured shell, **When** `kelir bg run "<cmd>"` is run, **Then** it prints a line `<id> running pid <pid>`, the output file path, and the three follow-up commands (`out`, `wait`, `cancel`), and exits 0.
2. **Given** a running job, **When** `kelir bg wait <id>` is run, **Then** it prints `<id> exited exit <code> after <secs>s` and exits 0 once settled; with `--timeout N` and the job still running it prints that the job is still running and exits 1.
3. **Given** a job whose command writes to stdout and stderr, **When** `kelir bg out <id>` runs, **Then** it prints everything written so far; with `--follow` it streams until the job has settled and no new bytes arrive.
4. **Given** a running job, **When** `kelir bg cancel <id>` runs, **Then** the job's whole process tree is killed and the job is recorded as `cancelled`; cancelling an already settled job says "nothing to cancel" and exits 0.
5. **Given** a job started from inside a session, **When** it settles (exited, cancelled or lost), **Then** the session receives a `user` message such as `[bg job <id> exited <code> after <secs>s]` with the command, output path, byte/line counts and the output tail, and the log records `call_settled{kind:"bg"}`.

---

### User Story 2 - A long bash call is promoted instead of killed (Priority: P1)

A `bash` tool call that outlives the promotion limit (default 600 s, `promote_after` / `KELIR_PROMOTE_AFTER`, `0` disables) becomes a background job. The tool result tells the agent the job id, how to read, wait for and cancel it, what was already printed, and that it will be told when it settles.

**Why this priority**: It turns "command timed out and lost its work" into "command keeps running and reports back", and is the default safety net for every agent.

**Independent Test**: Set `KELIR_PROMOTE_AFTER=1`, run a bash call of `sleep 3; echo hi`; verify the result is the "promoted to a background job" text and a wake message arrives later.

**Acceptance Scenarios**:

1. **Given** a bash call running past the limit, **When** the limit elapses, **Then** the result states "promoted to a background job after running past the Ns limit", names the job id, the three commands, bytes/lines printed so far and the output tail.
2. **Given** `promote_after = 0`, **When** a call runs long, **Then** it is not promoted and runs to completion.
3. **Given** a promoted job and a session that later ends, **Then** the job keeps running; a non-promoted bash call's tree is killed when its owning session exits.
4. **Given** a promoted job settles after a headless run already exited, **Then** the settlement is delivered on `kelir run --resume`.

---

### User Story 3 - Delegate work to a child session (Priority: P1)

An agent, from a `bash` call, starts another kelir session as a child (`kelir run --child "<task>"`), either blocking until its final text is printed on stdout, or in the background with a handle and a later wake-up message.

**Why this priority**: Delegation is the harness's only multi-agent mechanism (there is no spawn tool).

**Independent Test**: In a session, run `kelir run --child "say hi"` from a bash call; verify stdout is the child's final text, exit 0, and the parent's log has a `call_started`/`call_settled` pair of kind `child`.

**Acceptance Scenarios**:

1. **Given** a session, **When** `kelir run --child "<task>"` runs, **Then** the call blocks and stdout is the child's final text; exit 0 for `done`/`paused`, exit 1 (with `child <session> ended <outcome>` on stderr) for any other outcome.
2. **Given** `--background`, **When** the child is admitted, **Then** the command prints `child <chd_id> started in the background (session <id>) …Stop it with: kelir kill <session>` and exits 0 at once; when the child settles the parent gets a `user` message from `child` of the form `[child <session> <outcome>]` plus its answer (cut at 8000 bytes with a pointer to the child's log).
3. **Given** `--model`, `--provider`, `--cwd`, **When** omitted, **Then** the child uses the parent's current provider, model and cwd; a `--cwd` that is not a directory is refused.
4. **Given** no session (`KELIR_SOCK` unset), **When** `kelir run --child` runs, **Then** it is an ordinary run; `--child --background` prints that it needs a session and exits 2.
5. **Given** a bash call that is stopped while its foreground child runs, **Then** the child is killed and settles `stopped`.

---

### User Story 4 - Delegation is bounded and visible (Priority: P2)

Starts are refused when depth or concurrency caps would be exceeded; the family of sessions is visible as a tree and a parent can stop descendants.

**Why this priority**: Without caps an agent can fork-bomb the machine (the repo has a documented process-leak incident).

**Independent Test**: Set `[children] max_per_agent = 1`, start two background children; the second prints a `refused: …` line and exits 3. `kelir ls` shows the tree; `kelir kill <session>` stops one.

**Acceptance Scenarios**:

1. **Given** the defaults (depth 3, 10 live children per agent, 16 live descendants per root), **When** a start would exceed one, **Then** it is refused with exit 3 and one line naming the cap, the count and the config key.
2. **Given** two starts racing for the last slot, **Then** only one is admitted (the root decides in one step).
3. **Given** a settled child, **Then** its slot is free at once; a child whose process vanished is settled by its starter (`error`, or `killed` if signalled), or else reaped as `lost` by the root after a 5 s grace and its once-a-second tick.
4. **Given** a session, **When** `kelir ls` runs, **Then** it prints one line per session under `KELIR_ROOT` (id, state, depth, kind, model, cache-hit ratio, task); `--json` prints the same as a nested array; outside a session it shows the newest roots (10) and what is below them, `--all` shows everything.
5. **Given** a descendant, **When** `kelir kill <session>` (or a unique prefix of ≥ 8 chars, or a `chd_` id) runs in the session below which it lies, **Then** it and everything below it are stopped and settle `killed`; `kelir kill --tree` stops every descendant; naming oneself, a parent or a sibling is refused (exit 1).
6. **Given** a human in a terminal, **When** `kelir kill <session>` runs, **Then** it asks the tree's root using the root's token file; if the root cannot answer it signals the root's owner pid.

---

### User Story 5 - Ask a read-only side question with `brief` (Priority: P2)

`kelir brief "<question>"` asks a side question on a by-value snapshot of the current session, sharing its provider cache, without altering the parent's history.

**Why this priority**: A cheap way to consult the full context without polluting it or paying for a cold prefix.

**Independent Test**: In a session with history, run `kelir brief "what file did we edit?"`; verify stdout is the answer and the parent log gained only a `child` call pair.

**Acceptance Scenarios**:

1. **Given** a session, **When** `kelir brief` runs, **Then** a child session is seeded from the log up to the last settled tool batch, its request reuses the parent's system prompt, tools and messages plus one user message, with the parent's session id as the cache key.
2. **Given** a brief, **Then** `edit` and `compact` are refused by result, `bash` works, and `kelir run --child` / `kelir brief` from it are refused (exit 3).
3. **Given** a brief appears in `kelir ls`, **Then** its kind is `brief`, its task is its own question, and its tokens count only what it added after the snapshot.

---

### User Story 6 - Scheduled, named, single-flight runs (Priority: P2)

A scheduler (cron, timer, Task Scheduler) calls `kelir run --name <name> --single-flight --timeout D "task"`; the same session is resumed each time, overlapping runs skip, and the outcome is written to a STATUS file a script can read.

**Why this priority**: It is how kelir is operated unattended, with no daemon.

**Independent Test**: Run two `kelir run --name n --single-flight` concurrently; one exits 75 and writes a `skipped` STATUS; the other writes `done`.

**Acceptance Scenarios**:

1. **Given** a new valid name, **When** `kelir run --name <n> "task"` runs, **Then** a session is created and bound; the first task is a `user` event `from: "cli"`. Given an existing name, **Then** the session is resumed (log, model, tools, cwd from the log) and the new task is a `user` event `from: "job"`.
2. **Given** `--single-flight`, **When** another live run holds `names/<n>.lock`, **Then** this run exits 75 at once, writes a `skipped` STATUS naming the holder, and does not touch the holder's STATUS.
3. **Given** `--single-flight`, **Then** STATUS is written at start (`outcome: running`) and at the end with `done`/`paused` (exit 0), `stopped`/`error` (exit 1) or `timeout` (exit 124), with steps, tokens, cache-hit ratio, answer head, error and pid.
4. **Given** `--timeout 30m`, **When** the deadline passes, **Then** the turn is stopped, STATUS says `timeout`, the exit code is 124, and the process is forced out 20 s after the stop if it has not ended.
5. **Given** `--fresh`, **Then** the name is rebound to a new session whose context has a `carry` source holding the previous session's last compaction summary (else last answer), cut to 6000 chars; the old log remains.
6. **Given** a resume on a later UTC day, **Then** a `clock` source ("it is now <date>") is appended before the task; the same day appends nothing.
7. **Given** `--task-file PATH`, **Then** the task is read from the file; with neither words nor a file, from stdin.

---

### User Story 7 - Herdr shows agent state and pane context (Priority: P3)

Inside a Herdr terminal, kelir reports its working/idle/done/blocked/error state through terminal control records and tells the model which pane/space/tab it is in.

**Why this priority**: Pure observability for a multi-agent desktop; failing it must never fail a session.

**Independent Test**: With `KELIR_HERDR=1` run a one-shot task and capture stdout: it contains `ESC ] 7501 ; state=working ST` then `state=done`. With `HC_TERMINAL_ID` set and a stub `hc`, the system prompt contains one `herdr: pane …` line.

**Acceptance Scenarios**:

1. **Given** stdout is a terminal (or `KELIR_HERDR` is set and not `0|false|off|no`), **When** a turn starts and ends, **Then** `working` and then `done`/`error`/`blocked` (a paused turn) /`idle` records are written; `--json` never writes records.
2. **Given** `TERM_PROGRAM=herdr-telegram`, **Then** `done` is reported as `idle`.
3. **Given** a child session, **Then** it writes no Herdr status; only the root does.
4. **Given** `HC_TERMINAL_ID` is a valid pane address, **When** the session starts, **Then** `hc get <id> --json` (and `hc ls --json`) are consulted once, each within 1 s, and a single `herdr: pane <address>, space …, tab …, where "…"` line plus an agent-activity fragment is added to the frozen system prompt.
5. **Given** `hc` is missing, slow, fails or prints non-JSON, **Then** the line (or fragment) is omitted, a note is recorded, and the session continues.

---

### Edge Cases

- A job's shim is killed (kill -9, reboot) while `running`: readers report `lost`; `cancel` stops the leftover tree only when the pid can be proven to be the job's (start-time check), otherwise it refuses and signals nothing.
- A `cancel` is in flight: readers must not call the job `lost` during the window between the kill and the `cancelled` write (a marker valid for 30 s).
- A status file is torn or missing: listings skip it; `kelir bg wait` reports that the status file vanished (exit 1).
- A job id containing `/`, `\` or `..` is never resolved; unknown ids give "no background job" (exit 2).
- A command too long for an argument list is written to a script file in the job's directory and the status shows a truncated reference.
- `kelir bg run` with no command exits 2; the command cannot start (bad shell) → status has `error`, exit 1.
- A job started inside a session cannot choose its own id or learn the session's token; a spawn request that names an id is refused.
- Names: reserved suffixes (`.status.json`, `.lock`, `.stale.`), Windows device names, leading symbols, trailing dot and over-64-char names are refused (exit 2).
- A name whose session is running is refused naming the holder pid; an unreadable name file is treated as free.
- A stale lock (dead holder) is taken over only after the same verdict on two looks 150 ms apart, never for a lock younger than 1 s; an unreadable lock is respected until 30 s old.
- A resumed root settles its log's unsettled children as `lost`; a child whose parent died ends itself within ~0.5 s (Unix).
- A child result larger than 1 MiB is truncated and the CLI notes that the whole log is in the child session.
- A tool call that is not the root cannot reach the registry without a live-descendant session id; a refusal writes nothing.
- A Herdr title or label containing control characters or escape sequences is flattened so it cannot inject terminal records or prompt lines.

## Requirements *(mandatory)*

### Functional Requirements

**Background jobs**

- **FR-001**: System MUST provide `kelir bg run <cmd…>`, `ls`, `out [--follow]`, `wait [--timeout S]` and `cancel`; one argument is a shell command line, several are literal words quoted to preserve boundaries.
- **FR-002**: `bg run` MUST start the command under the configured shell via a detached shim process, wait up to 10 s for the first status, and print id, state, pid, output path and usage lines; it MUST exit 1 if the status has an error or never appears.
- **FR-003**: Each job MUST own one directory `<calls root>/<id>/` holding `<id>.status`, `<id>.out`, optional cancel/promote markers and command script; ids are `bg_` + 26 lowercase base-32 characters, or a call's output-dir name for a promoted call.
- **FR-004**: The shim MUST write the status file atomically (temp + rename) on each state change, record its own and the command's start times, send stdout and stderr to the output file, and strip provider key variables from the command's environment.
- **FR-005**: Job states MUST be `running`, `exited`, `cancelled` or `lost`; `lost` is a reader's verdict for a `running` status whose shim is gone or unprovable, and MUST NOT be written over a settled status.
- **FR-006**: `bg ls` MUST list only explicit jobs and promoted calls, newest first, with id, state, pid, exit code and the command cut to 60 chars (or "no background jobs in <dir>").
- **FR-007**: `bg cancel` MUST kill the whole tree (process group on Unix, Job Object on Windows) of a pid proven to be the shim, wait up to 2 s, then record `cancelled`; if the job settled meanwhile it MUST report that instead.
- **FR-008**: A session MUST watch its calls directory (every 250 ms) and deliver one `user` message and a `call_started`/`call_settled{kind:"bg"}` pair per job state change; the wake text names the outcome (`exited <code>`, `was cancelled`, `was lost`, `ended`), command, output path with byte/line counts and tail.
- **FR-009**: Inside a session, `bg run` MUST ask the session (`bg-spawn`) to start the shim, which mints the id, authenticates the caller by token and filters the caller's environment (≤ 4096 variables, ≤ 512 KiB, never the token or socket); outside a session the shim is started directly and belongs to nobody.
- **FR-010**: A bash call MUST be promoted to a background job when it runs longer than `promote_after` seconds (default 600; `KELIR_PROMOTE_AFTER` overrides; `0` disables); a promoted job survives the session, an unpromoted call is killed when its owner exits.

**Named and scheduled runs**

- **FR-011**: `kelir run --name <n>` MUST create or resume the session bound to the name, published create-or-fail so two processes cannot bind one name; `kelir ls sessions [--json] [--named] [--all]`, `ls rename` and `ls forget` MUST manage names.
- **FR-012**: `--single-flight` MUST require `--name`; `--timeout` MUST require `--single-flight`; `--task-file` MUST conflict with positional words; `--name` MUST conflict with `--resume` and `--child`.
- **FR-013**: With `--single-flight`, a per-name lock MUST be published atomically with its holder record (token, pid, start, cmd, started); only its owner may release it; a busy lock yields exit 75 and a `skipped` STATUS.
- **FR-014**: STATUS (`<state>/names/<name>.status.json`) MUST be rewritten atomically by each single-flight run with fields name, session, started, finished, outcome, exit_code, steps, tokens, cache_hit_ratio, answer, error, pid; the answer is the first ≤ 12 lines / 1200 chars.
- **FR-015**: Exit codes MUST be 0 done/paused, 1 error/stopped, 2 usage, 75 skipped, 124 timeout; durations accept `s m h d w` (case-insensitive), a bare number means seconds, unknown units and overflow are usage errors.
- **FR-016**: With `--json`, the event log MUST stream as NDJSON ending with a `result` event and the plain answer line MUST be suppressed.
- **FR-017**: `--fresh` MUST rebind the name to a new session with a `carry` source; resuming on a later UTC day MUST append a `clock` source; `KELIR_TEST_TODAY` MUST replace the clock.

**Child sessions**

- **FR-018**: `kelir run --child [--background] [--model] [--provider] [--cwd]`, `kelir brief`, `kelir ls`, `kelir kill <session>|--tree` MUST exist; there is no spawn tool. `--background` MUST require `--child`.
- **FR-019**: Every start MUST be admitted by the root's registry in one step against depth (`max_depth`, 3), live children per agent (`max_per_agent`, 10) and live descendants per root (`max_live_descendants`, 16); a refusal MUST be a single `refused: …` line and exit 3; an unknown parent is refused.
- **FR-020**: A child MUST run as its own kelir process with its own session dir and log in the parent's process group/Job Object, receive the task and the root token over stdin (never argv, environment, files or logs), and export `KELIR_ROOT`, `KELIR_PARENT`, `KELIR_DEPTH` to its tool calls.
- **FR-021**: The root MUST write `call_started`/`call_settled` with `kind:"child"` for every descendant it admits; a non-root parent writes the pair for its direct children; outcomes are `done`, `error`, `killed`, `stopped`, `lost` (`paused` counts as success); a kill aimed at a child wins over its exit.
- **FR-022**: A background child's settlement MUST become a `user` event `from:"child"`, `[child <session> <outcome>[ exit N]]` + result (cut at 8000 bytes with a pointer) or `(no final text)`; a foreground child's answer is the command's stdout, up to 1 MiB.
- **FR-023**: Registry operations from descendants (`admit`, `up`, `settle`, `kill`) MUST require the root token and a caller session that is a descendant of the root; `settle`/`up` for a child not owned by the caller are refused.
- **FR-024**: `kill` MUST only affect descendants of the asker, resolving by full id, `chd_` id or ≥ 8-char prefix; ambiguous targets are refused; the pid signalled is the registered one checked against its recorded start time.
- **FR-025**: A brief MUST copy the parent's log up to its last settled tool batch into a new child session, use the parent's session id as provider cache key, refuse `edit`, `compact` and further children, and leave the parent's history untouched except for the `child` call pair.
- **FR-026**: `kelir ls` MUST show each session's kind (`session`, `child`, `brief`), state (`running`/`idle`/…, `ended`, `crashed`, or the registry's outcome once finished), depth, model, cache-hit ratio and task; `--json` MUST include `id, parent, depth, kind, state, provider, model, cache_hit_ratio, task, children`.
- **FR-027**: A resumed root MUST settle its log's unsettled children as `lost`; a child whose parent process vanished MUST end itself (within ~0.5 s polling, forced exit after 5 s on Unix).

**Herdr**

- **FR-028**: Status writing MUST be enabled when `KELIR_HERDR` is set to anything but `0|false|off|no`, or when unset and stdout is a terminal, and MUST be disabled with `--json`; each record is exactly `ESC ] 7501 ; state=<working|done|idle|blocked|error> ESC \`.
- **FR-029**: A paused turn MUST be reported as `blocked`; a `herdr-telegram` terminal MUST report `done` as `idle`; write failures MUST be ignored.
- **FR-030**: A terminal title record (OSC 0) MUST strip control characters and be at most 80 characters.
- **FR-031**: Only the root session MUST write Herdr status; child sessions never do.
- **FR-032**: When `HC_TERMINAL_ID` is a valid address (≤ 64 chars of `[A-Za-z0-9._-]`, not starting with `-`), the session MUST run `hc get <id> --json` and `hc ls --json` once each with a 1 s limit and render one frozen line `herdr: pane P[, space S][, tab T][, where "W"]` plus `; agents N (working, idle, done, blocked[, other]), spaces S (+K other), machines M` when there is anything to count; labels are single-line, ≤ 80 chars, quotes replaced.
- **FR-033**: Any `hc` failure, timeout, or malformed JSON MUST omit the affected part, record a note, and never fail the session; outside Herdr no line and no note appear.

### Key Entities

- **Job / Status file**: one background command: id, state, pids, shim and command start times, exit code, started/finished, command (or script path), output path, kind (`job` or `call`), promoted flag, owning call, error.
- **Shim**: the detached `kelir bg-shim` process owning a job's tree and its status file.
- **Name**: a stable handle `names/<name>` → session id, with created/updated times.
- **Run lock and STATUS**: `names/<name>.lock` and `names/<name>.status.json` for single-flight runs.
- **Registry entry**: root-held record of a descendant: child id (`chd_…`), session, parent, depth, kind, background flag, task, model, pid and start time, outcome, killed flag.
- **Caps**: depth, per-agent and root-wide live limits.
- **Child note**: a settlement carried to the parent (outcome, exit code, result) that becomes a log event and, if background, a wake message.
- **Herdr record / pane line**: OSC status record; the one-line pane and activity description in the system prompt.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: `kelir bg run` returns an id to the caller in under 10 s in all cases (success or reported failure), independent of how long the command runs.
- **SC-002**: A settled job produces exactly one wake message and one `call_settled{kind:"bg"}` per state within one watcher interval (≈ 250 ms) of the status file changing.
- **SC-003**: No job is ever reported `running` after its shim is provably gone, and `cancel` never signals a process whose identity it cannot prove.
- **SC-004**: With N concurrent single-flight starts on one name, exactly one runs and N−1 exit 75 with a `skipped` STATUS; the holder's STATUS is never overwritten.
- **SC-005**: Under racing starts, live children never exceed any configured cap; every refusal is exit 3 with a one-line message naming cap and count.
- **SC-006**: A crashed or externally killed child frees its slot within about 6 s (5 s grace + 1 s tick), and a killed parent leaves no running Unix child after ~0.5 s plus teardown.
- **SC-007**: A brief leaves the parent's log unchanged except for one `child` start/settle pair, and its first request reuses the parent's cached prefix.
- **SC-008**: Herdr probing adds at most ~2 s (two 1-s limits) to session start and never causes a session failure.

## Assumptions

- Users run kelir on Windows 11 or Linux (macOS best-effort); on platforms that cannot report process start times a job is never signalled on the strength of a pid alone.
- The scheduler, not kelir, decides when a named run starts; kelir has no daemon or job configuration file.
- Children use their own provider credentials from the environment or credential store; none is passed in argv.
- Same-user processes that can read the root's token file are trusted as `kelir ctl` already is.
- Herdr is the external desktop app providing `hc` and `HC_TERMINAL_ID`; kelir only consumes them.
- `docs/reference/jobs.md` describes named/scheduled runs; background jobs (`kelir bg`) are documented in `cli.md`, `verbs.md` and `children.md`.

## Code vs docs discrepancies

1. `docs/reference/children.md:6-8` ("Read this when" cites `src/child.rs`, `src/childcli.rs`, `src/session/children.rs`): these files no longer exist; the code is `src/features/children/{mod,cli,feature,ops}.rs`.
2. `docs/reference/jobs.md:6-8` cites `src/namedrun.rs`, `src/names.rs`, `src/joblock.rs`: the code is `src/features/names/namedrun.rs`, `src/features/names/mod.rs`, `src/sys/joblock.rs`.
3. `docs/reference/config.md:41` cites `src/config.rs:promote_after`: the function is `src/config/resolve.rs:137`.
4. `docs/reference/children.md` (Caps section) says only a child "nobody reports on" is reaped after a 5 s grace; the code also reaps a child that was admitted but never reported a pid after 120 s (`src/features/children/mod.rs:37`, `:310`) and settles a killed one immediately.
5. `docs/reference/jobs.md` ("Flags"/"Single flight"): says `--fresh` and `--timeout` belong to a scheduled run; `src/core/session.rs:92` also routes `--fresh` alone (no `--single-flight`) through the named-run path, where no lock or STATUS is written (`src/features/names/namedrun.rs:414`, `:554`). Not documented.
6. `docs/reference/jobs.md` ("STATUS" table): `answer` is "the first lines"; the code keeps at most 12 lines and 1200 characters (`src/features/names/namedrun.rs:114`).

## Source map

- `src/features/bg/mod.rs`, `cli.rs`, `launch.rs`, `shim.rs`, `status.rs`, `text.rs`
- `src/features/children/mod.rs`, `cli.rs`, `feature.rs`, `ops.rs`
- `src/features/herdr/mod.rs`, `ctx.rs`
- `src/features/names/mod.rs`, `namedrun.rs`; `src/sys/joblock.rs`
- `src/core/session.rs`, `src/core/session/boot.rs`, `src/core/session/view.rs`, `src/core/session/verbloop.rs`, `src/core/session/report.rs`, `src/tools/bash.rs`
- `src/main.rs`, `src/main_cli.rs`, `src/config/resolve.rs`
- `docs/reference/jobs.md`, `docs/reference/children.md`, `docs/reference/cli.md`, `docs/reference/env.md`, `docs/reference/sources-and-skills.md`, `docs/reference/config.md`
- Tests (by name): `tests/it/bg*.rs`, `tests/it/tools_bg*.rs`, `tests/it/jobs*.rs`, `tests/it/children*.rs`, `tests/it/hint_herdr*.rs`
