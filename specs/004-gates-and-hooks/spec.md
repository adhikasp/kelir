# Feature Specification: Gates and Hooks

**Feature Branch**: `004-gates-and-hooks`

**Created**: 2026-11-27

**Status**: Reverse-engineered from main e1cd8ea

**Input**: User description: "Reverse-build the spec for kelir's gates: executables or built-in Rust gates that run at six named session events and may allow, deny or rewrite, with fail-open behaviour, trust rules, bounded process counts and a scrubbed environment."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Veto or nudge a session with an external gate (Priority: P1)

A person declares a command under `[gates]` in their config for one of six events. At that event kelir runs the command, sends it the event as JSON on stdin and reads one JSON decision from stdout: `allow`, `deny` or `rewrite`. Nothing is enabled by default; opinions live in gates, not in the core.

**Why this priority**: This is the whole feature. Without it the other stories have nothing to extend.

**Independent Test**: Declare `before_tool = "<script that prints a deny>"`, let the model call `bash`, and check the call is refused with `denied by gate <name>: <reason>` and one `gate` event is logged.

**Acceptance Scenarios**:

1. **Given** a `before_tool` gate that denies, **When** the model calls a tool, **Then** the call is not run and its result reads `denied by gate <name>: <reason>` (just `denied by gate <name>` when the reason is blank).
2. **Given** a `before_tool` gate that rewrites `input`, **When** the model calls a tool, **Then** the tool runs with the rewritten input object.
3. **Given** an `after_tool` gate that answers `rewrite` with `{"result": ...}` or `{"note": ...}`, **When** a call finishes `ok` or `error`, **Then** the model reads the replaced result, or the original result followed by a line `[gate <name>] <note>`.
4. **Given** a `prompt_submit` gate that denies, **When** the person submits a prompt, **Then** the prompt is dropped and `prompt dropped by gate <name>: <reason>` is shown to the person.
5. **Given** a `prompt_submit` gate that rewrites `{"text": ...}`, **When** a human prompt arrives, **Then** the rewritten text is what the session records and the model sees.
6. **Given** a `turn_end` gate that denies, **When** the model gives a final answer, **Then** the turn continues once, with the reason delivered as a message from `gate` ("Your answer was not accepted: ..."); a second deny in the same turn is logged as an `error` and not enforced.
7. **Given** a `turn_end` gate that rewrites `{"text": ...}`, **When** the final answer is produced, **Then** the rewritten text becomes the final text.
8. **Given** `session_start` and `session_end` gates, **When** a session begins and ends, **Then** each runs for side effects only.
9. **Given** several gates on one event, **When** the event fires, **Then** they run in declaration order, each seeing the previous rewrite, and the first `deny` (at an event where deny means something) ends the chain.

---

### User Story 2 - A broken gate never breaks the session (Priority: P1)

A gate that cannot start, exits non-zero, runs too long, prints garbage or rewrites with the wrong shape is treated as `allow`. The failure is recorded; the session carries on.

**Why this priority**: Gates run arbitrary code as the user. A buggy one must not wedge or kill a run.

**Independent Test**: Declare gates that exit 1, print `not json`, sleep past the timeout, print over 1 MiB, or answer `rewrite` without `input`. For each, the session proceeds and the `gate` event says `decision: allow` with an `error`.

**Acceptance Scenarios**:

1. **Given** a gate exiting non-zero, **When** it runs, **Then** the event is allowed and `error` reads `exited with <code>` plus the stderr tail (300 characters at most).
2. **Given** a gate that runs past 30 s, **When** the timeout hits, **Then** its whole process tree is killed, the run is allowed and `error` says `timed out after <ms> ms`.
3. **Given** a gate that detaches a daemon (`setsid`, double fork) and exits, **When** the run ends, **Then** no process of that run is left alive.
4. **Given** a gate that prints more than 1 MiB, **When** it ends, **Then** the run is allowed with `error: output over 1048576 bytes`.
5. **Given** a gate printing nothing, **When** it exits 0, **Then** it counts as `allow` with no error.
6. **Given** a `deny` at `session_start`, `after_tool` or `session_end`, **When** it is parsed, **Then** it is recorded as `deny` with the reason suffixed `(ignored: deny means nothing at <event>)` and nothing is blocked.
7. **Given** a `rewrite` at `session_start` or `session_end`, **When** it is parsed, **Then** the run is allowed and `error` reads `rewrite means nothing at <event>`.

---

### User Story 3 - Project gates need trust (Priority: P1)

A gate brought by a repository (its `.kelir/config.toml` `[gates]`, or a skill under `.kelir/skills` or `.claude/skills`) runs only if the person has declared that project trusted. Gates from the person's own machine config always run.

**Why this priority**: Cloning a repo must not execute its code without consent.

**Independent Test**: Put a gate in a project config, start a session in it with no trust, and see it skipped with an `error` event; list the directory in `trusted_projects` (machine config) and see it run.

**Acceptance Scenarios**:

1. **Given** an untrusted project with a project-layer gate, **When** the session starts, **Then** the gate is not started, an `error` event (`where` `gate`) reads "gate <name> (<origin>) not started: this project is not trusted ...", and `observe` marks it `trusted: false`.
2. **Given** the project directory (or a parent) listed in the machine config's `trusted_projects`, **When** the session starts, **Then** its project gates run.
3. **Given** `KELIR_TRUST_PROJECT` set to anything other than blank or `0`, **When** the session starts, **Then** project gates run for that run only.
4. **Given** `trusted_projects` inside a project config, **When** config loads, **Then** it is ignored with a warning (a project cannot trust itself).
5. **Given** machine config or `--config`/`KELIR_CONFIG` gates, **When** the project is untrusted, **Then** they still run.

---

### User Story 4 - Declare gates three ways, plus a built-in (Priority: P2)

A gate can be declared as an inline command string, as a table with options, or as a script bound by a skill's front matter; and a named built-in (`hint`) can be declared in place of a command.

**Why this priority**: Packaging gates in skills and filtering by tool makes gates practical to share.

**Independent Test**: Declare the same deny three ways (string, table, skill-bound script); each produces the same denial.

**Acceptance Scenarios**:

1. **Given** `[gates] turn_end = ["a", "b"]`, **When** config loads, **Then** both gates run in that order.
2. **Given** a table with `name`, `tools` (or `tool`) and `env`, **When** a `before_tool` fires for a tool matching an exact name, an `mcp:<server>` entry (every tool of that server) or a trailing-`*` prefix, **Then** the gate runs; for a non-matching tool it does not.
3. **Given** a skill whose `metadata` has `kelir-gate: turn_end`, **When** sources are discovered, **Then** `scripts/turn_end.*` (else `scripts/turn-end.*`, else `scripts/gate.*`, else the only file in `scripts/`) is bound and the gate is named `<skill>.turn_end`.
4. **Given** `kelir-gate: turn_end=scripts/check.py; after_tool=scripts/note.py`, **When** discovered, **Then** the named files are used; a path that is absolute or contains `..` is refused with a note.
5. **Given** two gates with the same name, **When** config loads, **Then** later ones are renamed `-2`, `-3`.
6. **Given** `[gates.x] builtin = "hint"` with a `model`, **When** a human prompt is submitted, **Then** the built-in runs in-process through the same runner, logs the same `gate` event and, when it finds useful skills, contributes a `context` naming them.

---

### User Story 5 - A gate cannot multiply or escape (Priority: P2)

Gate processes are capped, a gate never triggers gates, a gate never inherits secrets and no gate leaves a process behind, even when kelir itself is killed.

**Why this priority**: Gates are user code on a desktop. Runaway spawn and secret leakage are the failure modes that matter most.

**Independent Test**: Run a gate that spawns itself repeatedly; no more than 8 gate processes are ever alive and none remain afterwards. Print the gate's environment and check it has no `KELIR_TOKEN`, `KELIR_SOCK` or provider key.

**Acceptance Scenarios**:

1. **Given** 8 gate processes already alive in one kelir process, **When** another run starts, **Then** it is skipped and logged (`8 gates are already running; this run was skipped`), never queued or retried.
2. **Given** a child session (`kelir run --child`, `kelir brief`), **When** its gates run, **Then** it is limited to 2 live gate processes, uses its own session directory and log, and the whole tree (default 16 live descendants) stays within 8 + 16 × 2 = 40.
3. **Given** a kelir started by a gate (`KELIR_GATE_EVENT` set to non-blank), **When** it starts, **Then** it runs no gates.
4. **Given** any gate run, **When** it starts, **Then** its environment holds only the allow-list (`PATH`, home/temp/system, locale, proxy and CA variables, `SSH_AUTH_SOCK`), `KELIR_SESSION_DIR`, `KELIR_GATE_EVENT`, `KELIR_GATE_NAME`, `KELIR_GATE_RUN`, and the gate's own `env` table; never `KELIR_TOKEN`, `KELIR_SOCK` or any provider key.
5. **Given** kelir is killed while a gate runs (Unix), **When** the guard notices (about 0.2 s), **Then** the gate's process tree is killed.
6. **Given** a prompt a parent hands to a child (`from` `parent`, `brief`, `child`), a job wake-up (`job`) or a gate's own message (`gate`), **When** it is delivered, **Then** it passes through no `prompt_submit` gate.

---

### User Story 6 - Observe what gates did (Priority: P3)

The person can see which gates are in force and what they decided.

**Why this priority**: A veto nobody can explain is a bug report waiting to happen.

**Independent Test**: Run a session with one denying gate; `kelir observe` lists it and its run, and `ctl status` carries a `gates` object with `denied: 1`.

**Acceptance Scenarios**:

1. **Given** declared gates, **When** `kelir observe <session>` runs, **Then** it prints `== gates ==` (name, event, origin, command, tools, `env` variable names only, never values) and `== gates fired ==`.
2. **Given** a session with gates, **When** `status` is requested, **Then** the `gates` object carries `declared`, `runs`, `denied`, `rewritten`, `failed_open` and `last` (`gate`, `event`, `decision`).
3. **Given** every run (including failed-open and skipped ones), **When** it finishes, **Then** one `gate` event records `gate`, `event`, `decision`, `reason`, `ms`, `rewritten` and optional `error`.

---

### User Story 7 - Ready-made example gates (Priority: P3)

Five generic Python scripts ship under `examples/gates/` to copy. None is enabled by default.

**Why this priority**: They document the contract by example and are exercised by tests.

**Independent Test**: Pipe `{"event":"turn_end","final_text":"probably fine","tool_calls":0}` into `verify_before_claim.py` and get a deny.

**Acceptance Scenarios**:

1. **Given** a hedged final answer and no tool calls that turn (and not already blocked), **When** `verify_before_claim.py` runs, **Then** it denies once.
2. **Given** an edit with `NOTE_CHECK_CMD` in the gate's `env`, **When** `note_check.py` runs after an `ok` edit, **Then** it appends the checker's output as a note; with no checker, path or `ok` outcome it allows silently.
3. **Given** "do 1,4" after a numbered list, **When** `numbered_answers.py` runs at `prompt_submit`, **Then** it rewrites the prompt to the text of items 1 and 4.
4. **Given** a session that starts outside a repo, **When** `load_project_agents.py` sees the first `edit`/`bash` touching `PROJECT_ROOT` without its `AGENTS.md` loaded, **Then** it denies once with a read instruction; read-only bash is never denied.
5. **Given** `git_sync.py` at `session_start` and `session_end`, **When** it runs, **Then** it does `git pull --ff-only`, then commits tracked changes only and pushes, refusing the whole commit on a secret-like file name, honouring `GIT_SYNC_ADD_NEW`, `GIT_SYNC_DRY_RUN` and `GIT_SYNC_PUSH`, and never forcing.

---

### Edge Cases

- **Empty or blank command**: a config error at load (`an empty command`), not a runtime fail-open.
- **Unknown table key** in a gate table: config error naming the allowed keys; `tools` on an event other than `before_tool`/`after_tool` is a config error.
- **Invalid name or env variable name**: config error (`name` is letters, digits, `-`, `_`, `.`, at most 64 characters).
- **Unknown built-in or unsupported event**: config error listing the known built-ins (`hint`) or the supported event.
- **`reason` over 2000 characters**: cut to 2000 in the log and what the model reads.
- **`context` over 32 KiB**: cut; it is recorded as a `source` event of the answer's `kind` (default `gate`, at most 64 characters). At `prompt_submit` it is appended after the user message, elsewhere at the tail, so the cached prefix is unchanged.
- **`usage` answer** (object): recorded as a usage event whose `kind` is the gate's name; a non-object `usage`, non-string `reason` or non-string `context` fails the run open.
- **Gate that never reads stdin**, or that sends a verb to its own session: the session waits for the gate (at most 30 s), then proceeds; a `stop` is read only after the gate ends.
- **`after_tool` on a denied, stopped or otherwise unfinished call**: does not run; only outcomes `ok` and `error` are gated.
- **Rewritten `after_tool` result** is cut to the spill limit (never below 1024 bytes).
- **No shell configured**: the run fails open with `no shell configured`.
- **Command-line task dropped by `prompt_submit`**: a one-shot run ends with session end reason `denied` and exit 1; a TUI waits for the next message.
- **Hard kill of the session**: `session_end` gates do not fire.
- **Windows**: `.cmd`/`.ps1` gates work with a matching shell; no console window opens; the tree is owned by a kill-on-close job object.
- **macOS / other Unix**: the leftover sweep uses `ps` on macOS; on other Unix only the process group is killed. A gate that clears its own environment before detaching, or crosses `ssh` or a container, can escape the sweep.
- **`hint` built-in** with no key or an unreachable scorer: allows, and a note explains it is off or skipped; an empty prompt is allowed without a call.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST support exactly six gate events: `session_start`, `prompt_submit`, `before_tool`, `after_tool`, `turn_end`, `session_end`; event names are accepted case-insensitively with `-` for `_` when bound from a skill.
- **FR-002**: System MUST run no gates unless some are declared; a session with none pays no cost.
- **FR-003**: System MUST send each gate one JSON document on stdin carrying `event`, `session`, `session_dir`, `log` (path of `events.jsonl`), `cwd`, `turn` and the event's own fields (see FR-004).
- **FR-004**: The event fields MUST be: `session_start` → `provider`, `model`, `resumed`; `prompt_submit` → `text`, `from`; `before_tool` → `tool`, `call_id`, `input`; `after_tool` → `tool`, `call_id`, `input`, `result`, `outcome`, `exit_code`, `out_path`; `turn_end` → `final_text`, `tool_calls`, `steps`, `blocked`, `blocked_gates`; `session_end` → `reason`. `tool` is the native name or `mcp:<server>.<tool>`.
- **FR-005**: System MUST read a gate's answer as one JSON object with `decision` of `allow`, `deny` or `rewrite`; empty output is `allow`; `rewrite` requires an `input` object.
- **FR-006**: `deny` MUST be enforced only at `prompt_submit`, `before_tool` and `turn_end`; `rewrite` MUST be applied only at the four events that carry a payload, replacing: prompt text, tool input object, `{"result"}`/`{"note"}` for `after_tool`, final text for `turn_end`.
- **FR-007**: A `turn_end` deny MUST send the model back at most once per turn **by default**; a gate that raises its own budget (the `max_denies` field of its answer, spec 009's `timebox`) is enforced up to that many times, its own budget and not another gate's. `blocked` says whether any gate blocked this turn and `blocked_gates` names them; a deny past a gate's budget is logged, not enforced.
- **FR-008**: System MUST fail open: start failure, non-zero exit, timeout (30 s), invalid or oversized (over 1 MiB) output, and malformed rewrites all yield `allow` with the cause in the run's `error`.
- **FR-009**: A timed-out or finished gate's process tree MUST be killed; on Unix this includes any process still carrying the run's `KELIR_GATE_RUN` token; on Windows the tree is held by a job object; no console window may open.
- **FR-010**: On Unix, a `gate-guard` process started beside each gate MUST kill the gate's tree if kelir dies mid-gate.
- **FR-011**: System MUST allow at most 8 live gate processes per kelir process; a run past the cap is skipped and logged, never queued.
- **FR-012**: A child session MUST be limited to 2 live gate processes.
- **FR-013**: A kelir whose environment carries a non-blank `KELIR_GATE_EVENT` MUST run no gates.
- **FR-014**: System MUST give gates a scrubbed environment containing no provider key, `KELIR_TOKEN` or `KELIR_SOCK`; extra variables come only from the gate's `env` table.
- **FR-015**: Gates from machine config, `--config` and `KELIR_CONFIG` MUST always run; project-layer gates (project config, project skills) MUST run only when the project is trusted via `trusted_projects` (a directory covers everything under it) or `KELIR_TRUST_PROJECT`; untrusted ones MUST be skipped with one `error` event each.
- **FR-016**: Machine and project gates MUST both be kept (machine first); later same-named gates MUST be suffixed `-2`, `-3`.
- **FR-017**: A gate table MUST accept only `command`, `name`, `tool`/`tools`, `env`; `tools` is allowed only for `before_tool`/`after_tool`; an empty command is a config error.
- **FR-018**: A `before_tool`/`after_tool` gate with `tools` MUST run only for matching tools: exact name, `mcp:<server>` for all tools of that server, or a trailing-`*` prefix; with no `tools` it runs for every tool.
- **FR-019**: A skill MUST be able to bind a script as a gate via `metadata: {kelir-gate: ...}`; scripts MUST stay inside the skill directory; the interpreter follows the extension (`.py`, `.sh`, `.js`, `.ps1`, `.cmd`/`.bat`).
- **FR-020**: The command MUST run through the configured shell (adapted for `cmd.exe` or PowerShell).
- **FR-021**: A built-in gate MUST be declarable with `builtin = "<name>"`, produce the same `gate` event and obey the same fail-open and timeout rules; only `hint` exists, supports only `prompt_submit`, and requires `model`.
- **FR-022**: The `hint` built-in MUST accept `event`, `model`, `provider`, `endpoint`, `timeout_ms` (alias `timeout`), `max_names` (positive integers), `tools`, `name`; default timeout 2000 ms, default `max_names` 3, at most 64 candidate skills, scored by a provider call whose usage is logged with `kind` `hint`.
- **FR-023**: A gate answer's optional `context` (≤ 32 KiB) MUST be recorded as a `source` event after the user message at `prompt_submit` and at the tail otherwise; `reason` MUST be cut to 2000 characters.
- **FR-024**: `prompt_submit` MUST apply only to human prompts (command-line task, TUI, `ctl say`, channel) and MUST NOT apply to `job`, `gate`, `parent`, `brief` or `child` prompts.
- **FR-025**: A prompt dropped by a gate MUST show `prompt dropped by gate <name>[: <reason>]` to the person (stderr outside the TUI, even under `--json`).
- **FR-026**: A denied `before_tool` call MUST settle with outcome `denied` and text `denied by gate <name>[: <reason>]`, for native and MCP tools alike.
- **FR-027**: `after_tool` MUST run only for calls that ran and finished with outcome `ok` or `error`.
- **FR-028**: System MUST record one `gate` event per run, including failed-open and skipped runs, and expose gate counts and the last run in `status`, and effective gates and runs in `observe` (env values never printed).
- **FR-029**: Problems found while reading gate declarations (bad skill binding, untrusted skip) MUST be logged once as `error` events and never abort the session.
- **FR-030**: `session_start` gates MUST run before the command-line task reaches `prompt_submit`; `session_end` gates MUST run just before the session ends, including with reason `denied`.
- **FR-031**: The repo MUST ship five example gates (`verify_before_claim`, `note_check`, `git_sync`, `numbered_answers`, `load_project_agents`) in `examples/gates/`, Python 3 standard library only.

### Key Entities

- **Gate definition**: name, event, command or built-in name, built-in options, tool filter, extra `env`, origin (`config`, `skill <name>`, `project skill <name>`, project config).
- **Gate input**: the JSON event document (common fields plus event fields).
- **Gate answer**: `decision`, `reason`, optional `input`, `context`, `kind`, `usage`.
- **Run**: one gate execution as logged (`gate`, `event`, `decision`, `reason`, `ms`, `rewritten`, `error`).
- **Chain**: the runs of one event and the resulting verdict (allow, deny with gate and reason, or rewritten value), plus contributed contexts and usages.
- **Slot**: one of the capped live-gate-process counters (8 root, 2 child).
- **Trust declaration**: `trusted_projects` (machine config) or `KELIR_TRUST_PROJECT`.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A gate that fails in any listed way (exit, timeout, garbage, oversize, bad rewrite) never changes a session's outcome: 100% of those runs are logged as `allow` with an error.
- **SC-002**: No more than 8 gate processes are alive in one kelir process, and none from a run's temp directory remain after the run, including after a timeout, a detach, or the parent being killed.
- **SC-003**: Gates see no provider key, token or socket in 100% of runs.
- **SC-004**: A timed-out gate costs the session at most 30 s.
- **SC-005**: A session with no gates declared spawns zero gate processes and records zero `gate` events.
- **SC-006**: An untrusted project's gates execute zero times.

## Assumptions

- Users trust the code in their own machine config; trust is declared, not prompted (kelir has no prompt).
- Gates are Python, shell or any executable reachable through the configured shell.
- The built-in `hint` needs a provider key for the scorer unless an explicit `endpoint` is given.
- Process-tree cleanup on non-Linux/macOS Unix is best effort (process group only).
- A gate that talks to its own session through `kelir ctl` is a misuse; the supported alternatives are writing a file or returning `rewrite`.

## Code vs docs discrepancies

1. `docs/reference/gates.md:10` cites `src/gates.rs` and `src/session.rs` (`gate`, `gate_prompt`, `run_tools`, `drive_turn`); the code is in `src/features/gates/` (hooks in `feature.rs`, `core/session/toolloop.rs`, `turnloop.rs`).
2. `docs/reference/gates.md:26` heading says "three equivalent ways" but lists four items (the third, an inline string, is the string form of the first).
3. `docs/reference/gates.md:79` says the `hint` `provider` default is "the session's provider"; the code defaults to `openrouter` (`src/features/gates/builtin/hint.rs:59`).
4. `docs/reference/gates.md:103` says a rewritten `after_tool` result is cut to `spill_limit`; the code cuts to `max(spill_limit, 1024)` bytes (`src/core/session/toolloop.rs:289`).
5. `docs/reference/gates.md:65-70` implies a deny at `session_start`, `after_tool` or `session_end` does nothing; the code logs the run with `decision: deny` and an "(ignored: ...)" reason suffix (`src/features/gates/runner.rs:204-208`), and a `rewrite` there logs an `error` (`runner.rs:210-212`) rather than being silently inert.
6. `docs/reference/gates.md` (Limits) says "gate processes"; built-in gates also take one of the 8 slots (`src/features/gates/runner.rs:107-116`).
7. `src/features/gates/mod.rs:5` ends a doc comment mid-sentence ("never in the"), a cosmetic fault.

## Source map

- `src/features/gates/mod.rs`
- `src/features/gates/config.rs`
- `src/features/gates/decision.rs`
- `src/features/gates/runner.rs`
- `src/features/gates/limits.rs`
- `src/features/gates/feature.rs`
- `src/features/gates/builtin/mod.rs`
- `src/features/gates/builtin/hint.rs`
- `src/core/session/toolloop.rs`
- `src/core/session/turnloop.rs`
- `src/core/session/mainloop.rs`
- `src/core/session/lifecycle.rs`
- `src/core/session.rs`
- `src/tools/mod.rs`
- `src/config/load.rs`
- `src/config/types.rs`
- `src/ipc/observe.rs`
- `src/main_cli.rs`
- `docs/reference/gates.md`
- `examples/gates/`
- `tests/it/gates.rs`
- `tests/it/gates_adversarial.rs`
- `tests/it/gates_children.rs`
- `tests/it/gates_core.rs`
- `tests/it/gates_examples.rs`
- `tests/it/gates_observe.rs`
