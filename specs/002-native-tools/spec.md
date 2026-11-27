# Feature Specification: Native Tools, Base Prompt, Memory, Model Window and Session Names

**Feature Branch**: `002-native-tools`

**Created**: 2026-11-27

**Status**: Reverse-engineered from main e1cd8ea

**Input**: User description: "Reverse-build the spec for kelir's native tools (`bash`, `edit`, `compact`), their descriptions and output limits, the base prompt, the memory notes directory, model/window metadata, and session names."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - The model runs shell commands and gets a bounded result (Priority: P1)

The model calls `bash` with a command and a short human label. The command runs in the session's working directory under the configured shell. The model gets back the output, or, when the output is large, a compact notice with a file path, so one noisy command can never flood the context.

**Why this priority**: `bash` is the harness's only way to act on the world. Everything else (MCP, background jobs, child sessions) is a CLI reached through it.

**Independent Test**: Run a scripted model that calls `bash` with `printf hi`, then with a command that prints more than the spill limit. Check the result text and the call's output file.

**Acceptance Scenarios**:

1. **Given** a session with the default spill limit, **When** the model runs a command that prints a few lines and exits 0, **Then** the result is exactly the output and the call's outcome is `ok`.
2. **Given** a command that prints nothing and exits 0, **When** it settles, **Then** the result reads `[no output]`.
3. **Given** a command that exits non-zero, **When** it settles, **Then** the result begins with `[exit code N]`, the outcome is `error`, and the exit code is recorded on the call.
4. **Given** a command whose output exceeds `spill_limit` bytes, **When** it settles, **Then** the model sees a notice with the total bytes and lines, the path of the full output, the first 1024 bytes, the last 1024 bytes and a `sed -n` / `rg -n` hint. The full output is in the call's `out` file and the call is marked spilled.
5. **Given** two calls in one session, **When** each runs, **Then** neither sees shell state (cwd changes, variables, `cd`) from the other; each starts in the session cwd.
6. **Given** a command longer than 6000 characters, **When** it is run, **Then** it is written to a file and executed from there, so its length is not a limit.

---

### User Story 2 - The model edits files by exact replacement (Priority: P1)

The model calls `edit` with a path and either one `old`/`new` pair or an ordered `edits` list. Each `old` must match exactly once (unless `replace_all`). The call is all or none: a failing hunk leaves the file untouched. An empty `old` creates a file that does not exist yet.

**Why this priority**: `edit` is the safe, atomic way to change files. The prompt tells the model to prefer it over `python`/`sed` patches that can fail with exit 0.

**Independent Test**: Call `edit` on a temp file with a unique `old`, a repeated `old`, a missing `old` and a multi-hunk list where the last hunk fails, and check file bytes and result text.

**Acceptance Scenarios**:

1. **Given** an existing file, **When** `old` occurs exactly once, **Then** it is replaced and the result is `edited <path>: replaced 1 occurrence at line <n>`.
2. **Given** an `old` that occurs more than once and no `replace_all`, **When** the edit is called, **Then** it errors with the match count, "Nothing was written", and the file is unchanged.
3. **Given** `replace_all: true`, **When** `old` occurs several times, **Then** every occurrence is replaced and the result lists the line numbers (the first 20, then "… and N more").
4. **Given** an `edits` list of three hunks where the third `old` is not found, **When** the call runs, **Then** it errors naming "change 3 of 3", nothing is written, and hunks 1 and 2 are not on disk.
5. **Given** a path that does not exist, **When** the first hunk has an empty `old`, **Then** the file (and missing parent directories) is created with `new`, and later hunks in the same call edit that new text.
6. **Given** a file with CRLF line endings, **When** `old` is written with LF and the literal text does not occur, **Then** it is retried with the file's line endings and the replacement is converted the same way. A literal match always wins first.
7. **Given** a replacement identical to the text it replaces, **When** the edit runs, **Then** the file is not rewritten (mtime untouched) and the result says there was no change.

---

### User Story 3 - Every call carries a human label (Priority: P2)

Each native tool call includes a short `description`. Humans see that label in the transcript, the headless progress line and the live row, instead of the raw command or path. The raw arguments stay in the event log.

**Why this priority**: kelir is autonomous, so the human is a watcher. A glanceable intent line makes a long run readable without exposing a 40 KB command.

**Independent Test**: Run a scripted `bash` call with a `description` and assert the headless `→ bash` line shows the label while `events.jsonl` still holds the command.

**Acceptance Scenarios**:

1. **Given** a call with `description: "run the lighthouse audit"`, **When** it is shown, **Then** the label appears in place of the command and the command remains in the log.
2. **Given** a description of more than 120 characters, **When** it is shown, **Then** it is cut to 119 characters plus `…`.
3. **Given** a description that is missing, not a string, empty or only whitespace, **When** the call is shown, **Then** the display falls back to the command (`$ <command>`) or to `edit <path>`.
4. **Given** a description with control characters, newlines or an OSC introducer such as `]7501;`, **When** it is shown, **Then** control characters become spaces, whitespace is folded and the OSC introducer is dropped.
5. **Given** a `bash` call with no `description` at all, **When** it runs, **Then** it still runs; the label is only presentation.

---

### User Story 4 - A tool can be switched off mid-session without changing the request prefix (Priority: P2)

A human can disable or re-enable `bash`, `edit` or `compact` (or MCP tools) while a session runs. The tool list sent to the provider never changes, so the prompt cache stays valid. A call to a disabled tool returns a readable refusal.

**Why this priority**: it is the human's brake on an autonomous agent, and it must not cost cache hits.

**Independent Test**: Send the `tools --disable bash` verb, then script a `bash` call. Check the result and the provider request's tool list.

**Acceptance Scenarios**:

1. **Given** `bash` is disabled, **When** the model calls it, **Then** the outcome is `disabled` and the result says "disabled by user" and names `kelir ctl <session> tools --enable bash`.
2. **Given** a `before_tool` gate vetoes a call, **When** the call is settled, **Then** the outcome is `denied` and the result reads `denied by gate <name>: <reason>` (or `denied by gate <name>` when the reason is blank).
3. **Given** the tools verb is given an unknown tool name, **When** it is parsed, **Then** it is refused with the list of known tools.
4. **Given** the tool list at the first request and at the last, **When** compared, **Then** they are identical.

---

### User Story 5 - A stopped or long-running call is handled without losing its work (Priority: P2)

A call can be stopped by the user, skipped because the turn was stopped first, or promoted to a background job when it runs past `promote_after` (600 s by default; 0 disables). The model is told what happened and where the partial output is.

**Why this priority**: long builds and tests are normal. Neither a hung call nor a stopped call may wedge the turn or leave processes behind.

**Independent Test**: Run `sleep 5` with `promote_after = 1` and observe a `promoted` outcome. Stop another call mid-output and observe its partial-output text.

**Acceptance Scenarios**:

1. **Given** a call still running after `promote_after` seconds, **When** the shim acknowledges, **Then** the outcome is `promoted`, the model gets the job id, the output path, the counts and the last 1 KB, and the process keeps running with output still growing in the same file.
2. **Given** a call that is stopped while it has written output, **When** it settles, **Then** the outcome is `stopped` and the result reads `stopped by user[: reason]`, `partial output: <bytes> bytes, <lines> lines; full output at <path>`, then a `--- tail ---` block.
3. **Given** the turn is stopped before a call starts, **When** the call is reached, **Then** the outcome is `skipped` and the result begins `[Skipped] the turn was stopped before this call ran`.
4. **Given** the shell wrapper dies before the command settles, **When** the session notices, **Then** the outcome is `unknown`, the result says the outcome is unknown and the tree was stopped, and any partial output is reported.
5. **Given** a command that leaves a child running (`cmd &`), **When** the call settles, **Then** the call returns normally and the child stays alive, owned by the session.

---

### User Story 6 - A truncated command is flagged (Priority: P3)

When a shell cut a command short (a heredoc ended by end-of-file, "unexpected EOF"), the result carries a notice telling the model to write big files with `edit` and to check exit codes. A command that merely printed such a phrase is not flagged.

**Why this priority**: on Windows an over-long command line was silently cut with exit 0. The notice stops the model from trusting a half-written file.

**Independent Test**: Feed the output rendering a shell diagnostic like `bash: warning: here-document ... delimited by end-of-file` and a plain `git grep "unexpected EOF"` hit, and check which get the notice.

**Acceptance Scenarios**:

1. **Given** output with a line from a known shell (`bash:`, `sh:`, `zsh:`, `cmd`, `powershell`…) containing a truncation marker, **When** rendered, **Then** the `[kelir: your command may have been cut short…]` notice is appended.
2. **Given** a non-zero exit and a marker anywhere in the output, **When** rendered, **Then** the notice is appended.
3. **Given** a zero exit and a marker only in program output far from the end (not in the last 3 non-blank lines, not from a shell), **When** rendered, **Then** no notice is added.

---

### User Story 7 - Labelled read batches stay navigable when spilled (Priority: P3)

The prompt tells the model to join reads in one call, each labelled `echo === name`. If that output spills, the notice also lists every `=== label` with its byte offset, so a section that fell in the cut is still reachable.

**Why this priority**: it keeps the "one bash call per read round" habit useful even for large output.

**Independent Test**: Produce 50 KB of output with three `=== a`, `=== b`, `=== c` headers and check the spill notice's section list.

**Acceptance Scenarios**:

1. **Given** spilled output containing `=== name` lines, **When** the notice is built, **Then** it adds a line `--- sections …: name@offset, …`.
2. **Given** more than 40 headers, **When** the notice is built, **Then** only the first 40 are listed, followed by `, ...`.
3. **Given** a header longer than the label limit, **When** it is recorded, **Then** the label is cut to 40 bytes.

---

### User Story 8 - A fixed, small base prompt with the skills directories filled in (Priority: P2)

Every session starts from the built-in prompt: kelir is autonomous, with no approvals and no human in the loop. It names the three tools, says skills are pulled by reading a `SKILL.md`, and gives working rules for batching reads and for verifying builds. The `{{skills_dirs}}` placeholder is replaced by the configured skills directories. The prompt is rendered once and frozen.

**Why this priority**: the prompt is the one fixed part of every request prefix. Its size and stability set the cache hit rate.

**Independent Test**: Start a session, read the first request's system message and the `session_started` sources, and check the base prompt text and the substituted directories.

**Acceptance Scenarios**:

1. **Given** a default config, **When** a session starts, **Then** the system prompt begins with `# kelir` and contains the skills directories where the placeholder was.
2. **Given** a machine `prompt.md` in the config directory, **When** a session starts, **Then** it replaces the built-in prompt and `sources[]` records the file it came from.
3. **Given** a source file edited mid-session, **When** the next boundary comes, **Then** it arrives as a `source` message at the tail and the frozen prefix is not rewritten.

---

### User Story 9 - Memory notes persist across sessions (Priority: P2)

With `memory_dir` configured, the `.md` and `.txt` notes in it are loaded into the frozen system prompt under `# Memory`. The model writes memory with `edit` like any file. A note created or changed later arrives as a message at the next boundary. A deleted note produces a "disregard its earlier text" message.

**Why this priority**: it gives a session-less harness continuity without a database or daemon.

**Independent Test**: Point `memory_dir` at a temp directory with two notes, start a session, then have the scripted model `edit` a new note into the directory and check the next request.

**Acceptance Scenarios**:

1. **Given** `memory_dir` with notes `a.md` and `b.txt`, **When** a session starts, **Then** both are in the system prompt, in name order, and recorded in `sources[]` with kind `memory`.
2. **Given** a hidden file or a file with another extension, **When** the directory is scanned, **Then** it is ignored.
3. **Given** more than 64 files, **When** scanned, **Then** only the first 64 by name are read and a note says so.
4. **Given** a note the model creates mid-session, **When** the next boundary is reached, **Then** it arrives as a `memory` source message and the log shows `» new memory note: <path>`.
5. **Given** a relative `memory_dir`, **When** resolved, **Then** it is relative to the session cwd.
6. **Given** a loaded note is deleted, **When** the watcher sees it, **Then** a message says `memory note <name> was deleted; disregard its earlier text`.

---

### User Story 10 - The context window comes from the provider, with an honest fallback (Priority: P2)

kelir needs the model's context window for compaction. It resolves the window in a fixed order: a configured override, the provider's model list (live, then cached), then a 128000-token default. The model list is fetched beside the session and never blocks the start for more than a short grace. A `/model` swap re-resolves the window and tells the human when it changed.

**Why this priority**: a wrong window compacts too early or overflows the provider.

**Independent Test**: Serve a mock `/models` with a `context_length` for the model and check `state.window` and `window_source`. Repeat with the endpoint down.

**Acceptance Scenarios**:

1. **Given** a model with `model_windows` or `window` set, **When** the window is resolved, **Then** it is the configured number and the source is `config`, whatever the provider says.
2. **Given** no override and a model list that just answered with a context, **When** resolved, **Then** the source is `api`.
3. **Given** no override and a cached list younger than 24 h, **When** resolved, **Then** no fetch is started, the value comes from the cache and the source is `cache`.
4. **Given** no override, no cache and a failing endpoint, **When** resolved, **Then** the window is 128000, the source is `default`, and the banner line says `window 128000 tokens (default) · the model list did not answer` (or the short reason: `timeout`, `offline`, `malformed`, `http NNN`, `failed`).
5. **Given** a failed fetch, **When** the next start is within one hour, **Then** the failure is in backoff and no new request is made; the cached models are still used.
6. **Given** the model swap `provider/model`, **When** the prefix names a configured provider, **Then** that provider's key and client are used, the governor follows the provider, and a changed window is announced as `» window N tokens (source)`.
7. **Given** a model id that differs by case or only by vendor prefix from the list's id, **When** looked up, **Then** it still matches (exact, then case-insensitive, then last path segment with a compatible vendor).

---

### User Story 11 - Sessions have stable human names (Priority: P2)

`kelir run --name <name>` creates a session bound to that name, or resumes the one already bound. Names can be listed, renamed and forgotten with `kelir ls sessions|rename|forget`. A name that points at a running session is refused, and the message says who holds it.

**Why this priority**: scheduled and recurring work needs an address that outlives a session id.

**Independent Test**: Run with a new name twice (second resumes), start a second run while the first is live, then rename and forget.

**Acceptance Scenarios**:

1. **Given** a free name, **When** `run --name nightly` starts, **Then** a `ses_…` id is minted, the name is bound create-or-fail to it, and the session starts with exactly that id.
2. **Given** a name bound to a session with a log and no live owner, **When** run again, **Then** that session is resumed.
3. **Given** a name bound to a session whose owner process is alive, **When** run, **Then** it is refused with the session id and pid.
4. **Given** two processes naming the same free name at once, **When** both try, **Then** exactly one binds it and the other follows the winner's binding.
5. **Given** `ls rename old new` where `new` is taken, **When** run, **Then** it fails with "already taken" and `old` is unchanged.
6. **Given** `ls forget name`, **When** run, **Then** the name is dropped, the session and its log stay, and a missing name reports false rather than failing.
7. **Given** a name file that is corrupt, **When** read for a run, **Then** a warning is printed and the name is treated as free and rebound; **When** listed, **Then** it appears as a row with its reason instead of stopping the list.

### Edge Cases

- `bash` with a missing, non-string or blank `command` returns an `error` outcome `bash: missing non-empty \`command\``; no process is started.
- No shell configured returns `error` with "no shell configured". If the call directory cannot be created, the result is an `error`.
- An unknown tool name returns `unknown tool: <name>`. A dispatch never panics or fails the turn; every problem becomes a readable `error`.
- `edit` with both `old`/`new` and `edits`, with `edits` that is not an array, an empty `edits`, or neither form each returns a specific `error` and writes nothing.
- `edit` on a non-UTF-8 file refuses and points to `bash`. Nonexistent file with a non-empty first `old` returns "file not found". Empty `old` on an existing file errors. A later hunk with empty `old` on a file that does not exist yet errors.
- Non-zero spill boundary: output of exactly `spill_limit` bytes is inline; one byte more spills. Inline output keeps only the first `spill_limit` bytes in memory, so memory use stays about 11 KB however large the output.
- A final line without a trailing newline still counts as a line.
- A command that exits 0 but was cut short is still flagged by the truncation notice.
- Names: leading dot, trailing dot, uppercase, spaces, more than 64 characters, `con`/`prn`/`aux`/`nul`/`comN`/`lptN` (with any extension), and anything ending `.status.json`, `.lock`, containing `.stale.` or ending `.lock.stale` are refused. A name file whose session id is not a valid `ses_…` id is treated as unreadable so a tampered file cannot point outside `sessions/`.
- Memory: a file over 16 KB or past the 64 KB total budget is skipped with a note, not truncated.
- Models: a list entry with a zero context is ignored; a response with no `data` parses as an empty list; a fetch slower than 30 s is a timeout and is not remembered as a failure.

## Requirements *(mandatory)*

### Functional Requirements

**Tool set and schemas**

- **FR-001**: The native tool list MUST be exactly `bash`, `edit` and `compact`, in that order, and MUST be the same for the whole session.
- **FR-002**: `bash` MUST require `command` and `description` and forbid additional properties. `edit` MUST require `path` and `description`. `compact` MUST require `description` and accept an optional `focus`. All three MUST forbid additional properties.
- **FR-003**: The tool-call dispatcher MUST accept a `bash` call that has a non-blank `command` even when `description` is missing.
- **FR-004**: `bash` and `edit` descriptions MUST tell the model: no shell state carries between calls; output goes to a file and a result over the limit comes back as a notice with a path; commands over 6000 characters are run from a file; big files are created with `edit` and an empty `old`; `edit` is preferred to python/sed patches.
- **FR-005**: Any tool problem MUST become an `error` outcome with a readable message. A dispatch MUST NOT fail the turn.

**bash**

- **FR-006**: `bash` MUST run the command in the session's cwd (the `--cwd`, or a resumed session's recorded cwd), via the configured `shell` argv, with the session socket and token exported to the child as `KELIR_SOCK` and `KELIR_TOKEN`. The token MUST NOT be written to the log, to stdout or to an argv.
- **FR-007**: Every call MUST write its output to `calls/<dir>/out` under the session directory, with `<dir>` kelir's own `<seq>-<step>-<n>` name, never a provider-supplied id.
- **FR-008**: A command longer than 6000 characters MUST be written to a script file and run from there. On Windows the file route MUST always be used.
- **FR-009**: The result MUST be the output verbatim when it is at most `spill_limit` bytes (default 10240), prefixed by `[exit code N]` when the exit code is non-zero, or `[no output]` when empty.
- **FR-010**: Over the limit, the result MUST be a notice carrying total bytes, total lines, the output file path, the first 1024 bytes, the last 1024 bytes, an optional `=== label@offset` section map (at most 40 labels, each cut to 40 bytes), a truncation notice when it applies, and a `sed -n`/`rg -n` hint. The call MUST be marked `spilled`.
- **FR-011**: The call outcome MUST be `ok` for exit 0 and `error` for any other code. The exit code, byte count, line count and output path MUST be recorded.
- **FR-012**: The truncation notice MUST be added when a known-shell diagnostic line (or any line, if the exit is non-zero or it is among the last 3 non-blank lines) contains `delimited by end-of-file`, `unexpected EOF` or `unexpected end of file`.
- **FR-013**: A call still running after `promote_after` MUST be promoted to a background job (outcome `promoted`, with the job id, output path, counts and last 1 KB) and MUST NOT be killed. A promotion that is not acknowledged or whose command finished first MUST settle as a normal call.
- **FR-014**: A call whose task is cancelled MUST kill its whole process tree, but a child the command deliberately left running after a normal settlement MUST be kept alive and owned by the session.
- **FR-015**: A stopped call MUST report `stopped by user[: reason]` with the partial byte/line counts, the output path and the last bytes of output.
- **FR-016**: If the call's wrapper exits before the command settles, the outcome MUST be `unknown` and the process tree MUST be stopped.
- **FR-017**: On Windows a call's tree MUST be held in a named kill-on-close job object; a call whose job cannot be created MUST be refused rather than run unreachable.

**edit**

- **FR-018**: `edit` MUST accept exactly one of `old`/`new` (with optional `replace_all`) or a non-empty `edits` array of `{old, new, replace_all?}`; both or neither MUST be an error.
- **FR-019**: `edit` MUST apply hunks in order against the evolving text and MUST write the file only if every hunk succeeds.
- **FR-020**: A hunk's `old` MUST match exactly once unless `replace_all` is set. Zero matches and multiple matches MUST each be an error that says nothing was written.
- **FR-021**: An empty `old` MUST create a file that does not exist (including parent directories); only the first hunk may be empty, and later hunks edit the created text. An empty `old` on an existing file MUST be an error.
- **FR-022**: A path MUST be resolved against the session cwd unless absolute.
- **FR-023**: Matching MUST try the literal text first; only if absent MUST it retry with the file's line endings (CRLF file, LF pattern) or with LF when the pattern has CRLF, converting the replacement the same way.
- **FR-024**: A non-UTF-8 file MUST be refused with a pointer to `bash`.
- **FR-025**: The success text MUST be `edited <path>: replaced N occurrence(s) at line(s) …` (first 20 lines, then "… and N more"); a multi-hunk call MUST list one indented line per change under `edited <path>: N changes, all applied`. A call that leaves the bytes unchanged MUST NOT rewrite the file and MUST say so. A created file MUST report `created <path> (N bytes)`.
- **FR-026**: Every `edit` result MUST be saved to `calls/<dir>/out`.

**Display labels**

- **FR-027**: A call's label MUST be its trimmed, whitespace-folded `description`, with control characters replaced by spaces, OSC introducers (`]<digits>;`) removed and the length capped at 120 characters (119 plus `…`). A missing, non-string or blank description MUST fall back exactly to the command or path form. The raw arguments MUST stay in the log.

**Outcomes**

- **FR-028**: The outcomes a tool can produce MUST include `ok`, `error`, `stopped`, `skipped`, `denied`, `disabled`, `promoted` and `unknown`, each with a model-readable result string as specified in the stories above.
- **FR-029**: A disabled tool MUST return `disabled by user: the \`<name>\` tool is off for this session (…--enable <name>…)` and MUST NOT change the tool list sent to the provider. The tools verb MUST refuse an unknown name unless it is `mcp:<server>` or `mcp:<server>.<tool>`.

**Base prompt**

- **FR-030**: The built-in prompt MUST be at most 1 KB, MUST be rendered once at session start and frozen, and MUST substitute `{{skills_dirs}}` with the configured skills directories.
- **FR-031**: The prompt MUST state that there are no approvals and no human in the loop, that every tool call needs a short `description`, that `edit` is all-or-none, that skills are pulled by reading a `SKILL.md`, and that reasoning is dropped between turns (notes go to `.kelir/notes.md`).
- **FR-032**: A machine `prompt.md` in the config directory MUST replace the built-in prompt, and the source file MUST be recorded.

**Memory**

- **FR-033**: `memory_dir` MUST be unset by default. When set, files ending `.md` or `.txt` (case-insensitive extension, not starting with `.`) MUST be read in name order, at most 64 files, 16 KB per file and 64 KB total, into a `# Memory` section of the frozen system and recorded with kind `memory`. Anything left out MUST be named in a note.
- **FR-034**: A relative `memory_dir` MUST resolve against the session cwd.
- **FR-035**: A memory note created or changed after the start MUST arrive as a source message at the next boundary and MUST NOT rewrite the system prefix. A deleted note MUST produce a "disregard its earlier text" message.
- **FR-036**: A missing `memory_dir` MUST NOT be an error; an unreadable existing one MUST add a note.

**Model list and window**

- **FR-037**: The window MUST be resolved in this order: `model_windows[<model>]`, then `window` (both only when greater than zero; source `config`), then the provider's context for the model (`api` if fetched now, `cache` if from disk), then 128000 (`default`).
- **FR-038**: The model list MUST be cached per endpoint for 24 hours; a failed fetch MUST back off for one hour; a fetch MUST time out after 30 s; the start MUST wait at most 250 ms for it, then continue with the cache or default and let it finish beside the session.
- **FR-039**: The list parser MUST accept `context_length` or `limit.context` for the window, and derive tools, vision, parallel-call and reasoning support from `capabilities`, `supported_parameters` and `architecture.input_modalities`, treating unknown as unknown.
- **FR-040**: A model lookup MUST try exact id, then case-insensitive, then last path segment with a compatible vendor.
- **FR-041**: The banner MUST show a window line only when the window is neither `api` nor `config` and there is a reason, and never for a still-fetching list. A failure reason MUST be one of `timeout`, `offline`, `malformed`, `http NNN`, `failed`.
- **FR-042**: A model swap MUST resolve the new window from the cache without waiting on the network, start a background fetch, and log `» window N tokens (source)` only if the window or source changed. A swap to another configured provider MUST resolve that provider's key.
- **FR-043**: Session status MUST expose `window`, `window_source`, `window_note` and `models_fetching`.

**Session names**

- **FR-044**: A name MUST be 1 to 64 characters of `a-z`, `0-9`, `.`, `_`, `-`, starting with a letter or digit, not ending in a dot, not a Windows device name, and not a scheduled-run file name.
- **FR-045**: A name MUST be stored as one small JSON file `{session, created, updated}` in the state `names/` directory, published create-or-fail so concurrent first binders cannot both win, and replaced by atomic rename.
- **FR-046**: Running with a name MUST resume a bound session that has a log and no live owner, start with the bound id when it has no log yet, mint and bind a new id when the name is free, and refuse a name whose session is live, naming the session and pid.
- **FR-047**: The owner of a session MUST be recorded as pid plus start time; a pid whose start time no longer matches MUST NOT count as an owner.
- **FR-048**: `rename` MUST fail when the target name exists and when the source does not; `forget` MUST drop only the name; `list` MUST be sorted and MUST show an unreadable entry as a row with a reason.
- **FR-049**: A name file whose session id is not a valid `ses_[a-z0-9_]{1,60}` id MUST be rejected as unreadable.

### Key Entities

- **Tool call**: one invocation of `bash`, `edit` or `compact`. It has a kelir-minted call directory, an outcome, an optional exit code, byte and line counts, an output path, a spilled flag and the result text the model sees.
- **Call outcome**: `ok`, `error`, `stopped`, `skipped`, `denied`, `disabled`, `promoted` or `unknown`.
- **Capture**: the bounded in-memory view of a call's output (first `spill_limit` bytes, a 1 KB tail, counts, section labels).
- **Hunk**: one `{old, new, replace_all}` replacement inside an `edit` call.
- **Memory note**: a `.md`/`.txt` file in `memory_dir`, loaded as a `memory` source.
- **Model list cache**: per-endpoint list of model ids with context and capability flags, a fetch time, and an optional failure time and reason.
- **Window**: the context size in tokens, with its source (`config`, `api`, `cache`, `default`) and an optional note.
- **Name entry**: a name bound to one session id with created and updated stamps.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A command printing 1 GB uses no more than about 11 KB of kelir memory for its result, and the model's result is bounded to the first 1 KB, the last 1 KB and a notice.
- **SC-002**: A multi-hunk `edit` where any one hunk fails leaves the file byte-identical in 100% of cases.
- **SC-003**: A 40 KB command runs to completion with no truncation on both Unix and Windows.
- **SC-004**: The tool list in the first and last provider requests of a session is byte-identical, including after a tool is disabled.
- **SC-005**: A label for any `description`, however long or hostile, is at most 120 characters, on one line, with no control bytes.
- **SC-006**: A session start with a dead `/models` endpoint is delayed by no more than the 250 ms grace, and is never blocked by the 30 s timeout.
- **SC-007**: Of two processes racing to bind the same free name, exactly one wins in every run.
- **SC-008**: The built-in prompt stays at or below 1024 bytes.

## Assumptions

- The three native tools are the entire native surface; MCP, background jobs, child sessions and gates are reached as CLI commands through `bash` and are specified elsewhere.
- `compact` only requests compaction; the compaction itself happens at the next step boundary, and is specified with the context feature.
- The shell, `spill_limit`, `promote_after`, `window`, `model_windows` and `memory_dir` come from the config file; their parsing is specified with configuration.
- The process-ownership shim (`bg-shim`), the status file and job promotion belong to the background-jobs feature; this spec covers only what a `bash` call observably does with them.
- Scheduled single-flight runs (`--single-flight`, the STATUS record) use the same name files but belong to the jobs feature.
- Provider authentication and request retry are covered by the providers feature.

## Code vs docs discrepancies

1. `docs/reference/config.md:39`: says `window` is used "when a provider publishes none". `src/provider/models.rs` (`resolve`, first branch) and `src/config/resolve.rs:178` (`override_window`) make `window` an override that wins over provider metadata.
2. `docs/reference/config.md:35,39,40,44,67`: the `src/config.rs:load` source column. The code is in `src/config/load.rs`.
3. `docs/reference/files.md:99` cites `src/spill.rs` (now `src/log/spill.rs`), and `docs/reference/files.md:108-110` cite `src/names.rs` and `src/namedrun.rs` (now `src/features/names/mod.rs` and `namedrun.rs`).
4. `tests/it/tools_description_edges.rs:46-58`: a doc comment says ESC/OSC bytes survive the label sanitizer, but `src/tui/tools_view.rs:23-45` replaces control characters with spaces and drops OSC introducers.

## Source map

- `src/tools/mod.rs`
- `src/tools/bash.rs`
- `src/tools/edit.rs`
- `src/prompt.md`
- `src/log/spill.rs`
- `src/sys/shell.rs`
- `src/tui/tools_view.rs`
- `src/core/verbs.rs`
- `src/features/memory.rs`
- `src/features/sources/feature.rs`
- `src/features/sources/source/discover.rs`
- `src/features/sources/source/types.rs`
- `src/features/models.rs`
- `src/provider/models.rs`
- `src/config/resolve.rs`
- `src/config/load.rs`
- `src/config/types.rs`
- `src/core/compact.rs`
- `src/features/names/mod.rs`
- `src/features/names/namedrun.rs`
- `tests/it/tools_description.rs`
- `tests/it/tools_description_edges.rs`
- `docs/reference/config.md`
- `docs/reference/files.md`
- `docs/reference/sources-and-skills.md`
- `docs/reference/cli.md`
- `docs/SPEC.md`
