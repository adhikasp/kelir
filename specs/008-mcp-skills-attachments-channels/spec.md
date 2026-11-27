# Feature Specification: MCP, Skills/Sources, Attachments, Channel Bridges and Cross-Platform Process Plumbing

**Feature Branch**: `008-mcp-skills-attachments-channels`

**Created**: 2026-11-27

**Status**: Reverse-engineered from main e1cd8ea

**Input**: User description: "Reverse-build the spec for kelir's MCP client (`kelir mcp`), sources and skills discovery, image/audio/video attachments (`kelir view`), channel mode (`kelir channel`, chat bridges as MCP servers) and the cross-platform process layer (`src/sys`)."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - The model uses an MCP server through a CLI (Priority: P1)

An operator configures an MCP server in `config.toml`. The model never receives MCP tool schemas in its request. It runs `kelir mcp <server> <tool> k=v …` from a `bash` call, reads a schema only when it needs one (`ls`, `--schema`), and pipes the result into other commands.

**Why this priority**: MCP is the extension point for tools. Keeping schemas out of the request keeps the cached prefix small and stable.

**Independent Test**: Configure one stdio server with two tools, run `kelir mcp ls`, `kelir mcp <server> ls`, then a call with typed arguments, and check stdout, exit code and the `call_started`/`call_settled` pair in the log.

**Acceptance Scenarios**:

1. **Given** a configured server, **When** `kelir mcp ls` runs, **Then** it lists each server with name, state, tool count and a one-line description, and starts nothing.
2. **Given** a tool whose `inputSchema` property accepts a string, **When** the model passes `id=007`, **Then** the server receives the string `"007"`. A property typed as integer receives `3` for `n=3`. Anything that parses as JSON and is not string-typed is sent as that JSON value.
3. **Given** arguments that violate the schema (wrong type, missing `required`, value outside `enum`, extra property under `additionalProperties: false`, out-of-range `minimum`/`maximum`), **When** the call runs, **Then** every problem is listed, the exit code is 2, and the server is never asked.
4. **Given** a tool that returns `structuredContent`, **When** it is called, **Then** stdout is that JSON compact. Otherwise stdout is the text blocks joined by newlines. `--envelope` prints the whole result object.
5. **Given** a tool that answers `isError`, **When** it is called, **Then** its text goes to stderr and the exit code is 1, so `| jq` never parses an error as data.
6. **Given** a tool result with images, audio or binary resources, **When** it is rendered, **Then** they are decoded to files and the file paths are printed. Paths go on stderr when stdout is JSON that a pipe will parse.
7. **Given** a tool actually named `ls` or `call`, **When** the model needs it, **Then** `kelir mcp <server> call <tool> …` reaches it.

---

### User Story 2 - MCP servers are safe, bounded and logged (Priority: P1)

Inside a session, every `kelir mcp` request goes to the session process (the broker). The broker starts servers lazily, keeps them for the session, logs each call, hides secrets, and takes the servers down with the session.

**Why this priority**: Servers run arbitrary commands with secrets in their environment. A leak, a leftover process or an unlogged call is a security and reliability failure.

**Independent Test**: Run a session that calls a server, kill the session with SIGKILL, and check that no server or grandchild process survives. Grep the log for a canary value placed in the server's `env`.

**Acceptance Scenarios**:

1. **Given** a configured server not yet used, **When** the first call needs it, **Then** it is started, kept for the session, and restarted by the next call if it died.
2. **Given** any call, **When** it settles, **Then** the log holds a `call_started`/`call_settled` pair with `kind: "mcp"` and `info.name = "<server>/<tool>"`. The recorded input keeps argument keys and replaces string values with `<string, N bytes>`.
3. **Given** a call that gets no answer within the server's `timeout` (default 60 s), **When** the time runs out, **Then** the exit code is 4, the outcome is unknown, and the call is never retried.
4. **Given** a server that dies while starting, **When** a call needs it, **Then** the exit code is 3 and the message carries the last line of the server's stderr with every `env` value of 6 or more bytes replaced by `***`.
5. **Given** a session killed mid-call and then resumed, **When** the log is replayed, **Then** the call is settled as `outcome: "unknown"` (exit 4) and is not re-run.
6. **Given** a `kelir mcp` request from a bash child, **When** it carries no session token or a wrong one, **Then** the session refuses it before logging or starting anything, and the CLI exits 3 instead of falling back to an unlogged one-shot call.
7. **Given** `kelir ctl <session> tools '{"disable":"mcp:<server>.<tool>"}'` (or `mcp:<server>`), **When** the model calls a disabled tool, **Then** the call is logged and answered `disabled by user` with exit 1.
8. **Given** `kelir mcp` run outside a session (no `KELIR_SOCK`), **When** a command runs, **Then** the CLI starts the server for that one command and kills its process tree when the command ends.

---

### User Story 3 - Instructions, skills and memory reach the model (Priority: P1)

At session start the system prompt is rendered once and frozen: the base prompt, `AGENTS.md`/`CLAUDE.md` files, an optional memory section and an environment block. Skills are listed by directory and by builtin name but their bodies are pulled on demand. A source that changes on disk after the start arrives as a message at the tail, never as a rewrite of the frozen prefix.

**Why this priority**: The model must know the operator's rules, and the provider's prompt cache must survive edits.

**Independent Test**: Start a session in a repo with an `AGENTS.md` and a skill. Edit both mid-session. The next request is byte-identical up to the tail and ends with `source` messages announcing the edits.

**Acceptance Scenarios**:

1. **Given** `AGENTS.md`/`CLAUDE.md` in the config dir and in the cwd and its ancestors, **When** the session starts, **Then** all are rendered under `# Instructions`, config dir first, then nearest ancestor first. The walk up stops at the directory holding `.git` and at the home directory.
2. **Given** skills directories (`~/.claude/skills`, `<config>/skills`, each `skills_dirs` entry, `<cwd>/.claude/skills`, `<cwd>/.kelir/skills`), **When** discovery runs, **Then** each subdirectory with a valid `SKILL.md` (front matter `name`, `description`) is a skill named after its directory. The prompt names the directories and the builtin skills, never a skill body.
3. **Given** a project skill with the same name as a machine skill, **When** discovery merges, **Then** the project one replaces the machine one. This includes the builtin `kelir`, `delegation` and `herdr` skills, which sit in the machine layer and have no file.
4. **Given** an instruction file, memory note or skill edited on disk, **When** the next boundary comes, **Then** one `source` message carries the new content. Editing back to an earlier body is announced again. A deletion is announced once.
5. **Given** a compaction that folded away a source message, **When** the next boundary comes, **Then** the source is announced again.
6. **Given** `memory_dir` is configured, **When** the session starts, **Then** its notes are rendered under `# Memory` in the frozen system. A note changed or created later arrives as a `source` of kind `memory`.
7. **Given** `[gates.hint]` with a `model`, **When** a human prompt arrives, **Then** one scoring call may append `might be relevant: name - description` as a `hint` source at the tail. Any failure means no hint and at most one `error` note.

---

### User Story 4 - The model looks at an image or notes an audio/video file (Priority: P2)

`kelir view <path>` copies a file into the session's `attachments/` folder and queues it as a `source`. An image is sent as one `image_url` part of the last message, so earlier bytes and the cache prefix stay unchanged. Audio and video are stored with a one-line note and sent to no provider.

**Why this priority**: Image bytes in a prompt are paid for on every later request. A pointer plus an explicit `view` keeps costs and the cache predictable.

**Independent Test**: Run `kelir view a.png` in a session on a vision model, then inspect the next request body and the `source` event of kind `attachment`.

**Acceptance Scenarios**:

1. **Given** a valid png/jpg/gif/webp of 10 MB or less, **When** `kelir view` runs, **Then** the file is copied into `<session>/attachments/` and a `source` of kind `attachment` is recorded.
2. **Given** a missing file, a directory, an over-10 MB image, an unknown type, bytes that are not an image despite the extension, or a model the provider marks text-only, **When** `kelir view` runs, **Then** it prints a message and exits 1, and nothing is recorded.
3. **Given** a PDF, **When** `kelir view` runs, **Then** it is refused with a hint to extract text or render pages to png.
4. **Given** an audio or video file of 256 MB or less, **When** `kelir view` runs, **Then** it is copied to `attachments/` and an `attachment-note` source says where it is and that its content is not in the conversation.
5. **Given** the path `attachments/1-x.png`, **When** it is resolved, **Then** the session directory is tried before the cwd.
6. **Given** a model whose vision support the provider does not state, **When** an image is viewed, **Then** the view is allowed.

---

### User Story 5 - A chat bridge drives kelir (Priority: P2)

`kelir channel` runs headless. It connects every `channel = true` MCP server (or the ones named with `--server`), takes inbound chat messages, runs one named session per conversation, replies through the bridge's reply tool, and never answers one conversation twice in parallel.

**Why this priority**: It lets a remote user talk to the agent from a chat app without kelir shipping any chat protocol.

**Independent Test**: Run `kelir channel --max-messages 1` against a scripted MCP server that sends one `notifications/claude/channel` and records tool calls. Check one `reply` call, one `turn_done` notification and a `done` record in the spool.

**Acceptance Scenarios**:

1. **Given** a channel server, **When** kelir connects, **Then** `initialize` declares the configured capabilities (default `claude/channel`). Any capability containing `permission` or `approval` is refused in config and filtered at `initialize`.
2. **Given** an inbound notification with `conversation_id`, `message_id`, text and optional sender, attachments and `reply_to`, **When** it arrives, **Then** it is appended to `spool/<server>.jsonl` and fsync'd before anything else, and a repeated message id is ignored.
3. **Given** a message, **When** its turn runs, **Then** the model sees an envelope naming server, conversation and sender. The envelope says the sender is a remote user, not the operator, and that the text is untrusted data. The `user` event has `from: channel`.
4. **Given** a finished turn, **When** kelir answers, **Then** one call to the reply tool carries `conversation_id`, `text`, `message_id` and `reply_to` (the inbound `reply_to`, else its own id), followed by a `turn_done` notification and a `done` spool record.
5. **Given** a failed, paused or timed-out turn, **When** kelir answers, **Then** one fallback text is sent instead (the configured `fallback`, or a timeout or policy-blocked default).
6. **Given** a second message for a conversation with a running turn, **When** it arrives, **Then** it waits. At most `max_active` conversations run at once.
7. **Given** a crash mid-turn, **When** `kelir channel` restarts, **Then** unfinished messages are replayed in message-id order, and a message whose `done` was recorded is not answered again.
8. **Given** a chat message whose first line is `/stop`, **When** it is processed, **Then** it is sent as the `stop` verb to that conversation's session, never to the model as a prompt, and the reply text is `stopped`.

---

### User Story 6 - Child processes behave the same on Linux and Windows (Priority: P2)

Everything kelir starts for gates, hooks, credential helpers and MCP servers goes through one runner. The runner scrubs the environment, groups the process tree, caps output, enforces timeouts and kills the whole tree on drop or timeout. Shell commands that are too long, or any shell command on Windows, reach the shell as a script file.

**Why this priority**: The dev machine is a shared desktop. Leaked processes, visible windows and silently truncated commands have caused real incidents (see AGENTS.md).

**Independent Test**: Start a child that spawns a grandchild, drop or time out the run, and assert that no process from that tree remains.

**Acceptance Scenarios**:

1. **Given** a child started by the runner, **When** it exits, times out or the runner is dropped, **Then** its whole tree is killed (process group on Unix, Job Object on Windows).
2. **Given** a scrubbed environment, **When** a child starts, **Then** it inherits only allow-listed names plus the caller's own variables, not `KELIR_TOKEN`, `KELIR_SOCK` or provider keys.
3. **Given** a shell command longer than 6,000 bytes on Unix, or any command on Windows, **When** it is dispatched, **Then** it is written to `cmd.sh`, `cmd.bat` or `cmd.ps1` in the call's own directory and the shell runs that file.
4. **Given** a job name, **When** two runs try to hold its lock, **Then** one wins and the other learns the holder. A dead holder's lock is taken over only after two agreeing looks 150 ms apart, and never while it is under 1 s old.
5. **Given** Ctrl-C or SIGTERM during a wait, **When** it arrives, **Then** the turn ends as `stopped`. A second interrupt within 2 s exits with code 130.
6. **Given** a session socket, pipe and token file, **When** they are created, **Then** they are owner-only (Windows: owner-only DACL on the pipe and token).

---

### Edge Cases

- A `config.toml` syntax or type error prints file, line, column and the parser's message, never the source line or value, because `env` holds secrets and error text reaches the model and the log. A bad file does not stop a session; `kelir mcp` prints the error and exits 2.
- A server name must be letters, digits, `-` or `_`, and not `ls`. An unknown key in a server table, `timeout = 0`, or an empty `command` is an error that names the server.
- A project's `[mcp.servers.x]` replaces the machine's whole table, never field by field.
- Editing `config.toml` does not affect a running session, which reads both layers once.
- A request that fails after reaching the session is exit 4, not a retry. Only a failed connect falls back to starting the server in the CLI.
- An integer argument too big for a 64-bit integer stays text instead of being rounded.
- A channel server that cannot connect at session start is logged as an `error` event (`where: mcp`). `kelir channel` retries unavailable bridges.
- A channel notification that is malformed, has an empty text, text over 64 KB, an id, conversation or sender over 128 bytes or with control characters, more than 8 attachments, or a base64 attachment over 16 MB is dropped. It is logged once per shape and never echoed.
- A channel notification arriving between a tool call and its result waits for the next safe boundary, so a call's results stay contiguous.
- A torn tail of the spool (partial write, invalid UTF-8) is repaired on open, and the removed bytes are kept in a `.torn` file beside it.
- An instruction file over 64 KB is cut with a visible truncation marker. A file that would push the 256 KB total over budget is skipped with a note, not truncated.
- A skill directory whose `SKILL.md` is malformed is skipped with an `error` note. Names starting with `.` are ignored.
- `HC_TERMINAL_ID` unset means no Herdr line. A slow (over 1 s), failing or non-JSON `hc` leaves the line out with one `error` note.
- In a pane with `TERM_PROGRAM=herdr-telegram`, a finished job reports `idle`, not `done`.
- A `/stop` chat message is queued like any other message of its conversation, so it is handled when that conversation's current turn ends.

## Requirements *(mandatory)*

### Functional Requirements

**MCP configuration**

- **FR-001**: System MUST read MCP servers from two `config.toml` layers (machine, then the nearest `.kelir/config.toml` walking up from the cwd) and merge them by server name, a project entry replacing the machine one whole. `KELIR_CONFIG` or `--config` replaces the machine path.
- **FR-002**: System MUST support only the stdio transport and these per-server keys: `command` (required), `args`, `env`, `description` (first line only), `channel`, `timeout` (seconds, default 60, 0 refused), `reply_tool`, `declare_capabilities`. Unknown keys MUST be an error naming the file.
- **FR-003**: System MUST NOT quote the config source in errors, and MUST NOT print, log or echo `env` values anywhere (errors, logs, command line).
- **FR-004**: System MUST start a server with the scrubbed allow-list environment plus the server's own `env`, never the full parent environment.
- **FR-005**: On Windows a bare command name MUST resolve to `.exe`, `.cmd` or `.bat` on `PATH`. On Unix a command not found on `PATH` MUST be reported as unavailable.

**MCP commands**

- **FR-006**: System MUST provide `kelir mcp ls [--json]`, `kelir mcp <server> ls [--detail name|desc|full]` and `kelir mcp <server> <tool> [k=v …] [--json '{…}'|-] [--schema] [--envelope]`. `k=v` pairs win over `--json`.
- **FR-007**: System MUST type `k=v` from the tool's `inputSchema` (string-accepting properties keep text; others are JSON when they parse, else strings).
- **FR-008**: System MUST validate arguments against the schema (`type`, `enum`, `required`, `properties`, `additionalProperties: false`, `items`, `minimum`/`maximum`, `anyOf`/`oneOf`) before calling, list every problem, and exit 2.
- **FR-009**: System MUST use these exit codes: 0 ok; 1 tool `isError` or JSON-RPC error; 2 usage (bad words, unknown server or tool, schema failure, bad config); 3 server unavailable; 4 timeout or unknown outcome.
- **FR-010**: System MUST decode non-text result content (images, audio, binary) to files under the call's directory and print the paths.
- **FR-011**: System MUST speak a JSON-RPC subset (`initialize`, `tools/list`, `tools/call`, `notifications/cancelled`, `ping`, inbound notifications) with a 60 s `initialize` timeout, and re-read the tool list after `notifications/tools/list_changed`.

**Broker**

- **FR-012**: Inside a session (`KELIR_SOCK` set), System MUST route `kelir mcp` to the session's broker, start servers lazily, keep them for the session, serialise starting a server and run calls to one server concurrently.
- **FR-013**: System MUST log each call as `call_started`/`call_settled` with `kind: "mcp"`. Large results are written to `out_path`, not inline.
- **FR-014**: System MUST require the session token (`KELIR_TOKEN`) on every `mcp` request inside a session, and MUST refuse internal `mcp-settle`/`mcp-notify` verbs that lack the per-session key.
- **FR-015**: System MUST kill every server's process tree when the session ends or is killed (Unix: a watcher in the process group; Windows: a kill-on-close Job Object).
- **FR-016**: System MUST honour `ctl … tools {"disable":"mcp:<server>[.<tool>]"}` by answering `disabled by user` with exit 1.
- **FR-017**: System MUST NOT retry a call whose outcome is unknown.
- **FR-018**: System MUST run `before_tool`/`after_tool` gates around an MCP call.

**Channel servers inside a normal session**

- **FR-019**: A `channel = true` server MUST be connected when the session starts, declaring its configured capabilities (default `claude/channel`) at `initialize`. Non-channel servers declare none.
- **FR-020**: A `notifications/claude/channel…` notification MUST be written as a `source` event of `kind: "channel"` with the server as `name` at the next safe boundary. It is context, not a wake. A server that cannot connect MUST produce an `error` event.

**Sources and skills**

- **FR-021**: System MUST render the system prompt once at session start from the base prompt, instruction files, memory and environment, and MUST NOT rewrite it afterwards.
- **FR-022**: Base prompt precedence MUST be: `<cwd>/.kelir/prompt.md`, then the config's `prompt` file or `<config>/prompt.md`, then the embedded default. The `{{skills_dirs}}` placeholder names the skills directories. If absent they are appended under `# Skills`. The embedded prompt MUST be 1 KB or under.
- **FR-023**: System MUST read `AGENTS.md` and `CLAUDE.md` (config dir, then cwd and ancestors up to the repo root or home), 64 KB per file and 256 KB in total, decoding with BOM dropped and invalid UTF-8 replaced and reported.
- **FR-024**: System MUST discover skills from the five directory layers in order, accept only directories with a valid `SKILL.md` of 64 KB or less, and name skills after their directory. A project skill MUST replace a machine one of the same name.
- **FR-025**: System MUST ship three builtin skills (`kelir`, `delegation`, `herdr`) listed by name and description with a read command (`kelir docs --skill <name>`), recorded with an empty path and never watched.
- **FR-026**: System MUST record every source (kind, name, path, hash) in `session_started.sources[]` and compare content hashes at every boundary, announcing a change as one `source` message at the tail. It MUST re-announce a source whose earlier announcement was folded by compaction.
- **FR-027**: System MUST support `source` kinds `hint`, `skill`/`instruction`, `memory`, `attachment`, `attachment-note` and `channel`.
- **FR-028**: When `HC_TERMINAL_ID` is set, System MUST add one `herdr:` line to the frozen environment block from `hc get`, bounded to 1 s, labels one line of at most 80 characters.
- **FR-029**: `memory_dir` notes (`.md`/`.txt`, at most 64 files, 16 KB each, 64 KB total) MUST be rendered into `# Memory` and watched like other sources.
- **FR-030**: The hint gate MUST be off unless `[gates.hint].model` is set. It MUST make one scoring call per human prompt, fail open, drop names outside the catalogue, send the key only to the provider's own origin, and log spend as `usage` of `kind: "hint"`.
- **FR-031**: `kelir observe <session> [--raw]` MUST print what the model will see next. `--raw` MUST print only the exact JSON body.

**Attachments**

- **FR-032**: `kelir view` MUST accept png, jpg, jpeg, gif and webp up to 10 MB, verify the file signature (not just the extension), reject text-only models, and copy the image into `<session>/attachments/`.
- **FR-033**: System MUST send an image as an `image_url` part (`data:` URL with the sniffed media type) on the last message only.
- **FR-034**: `kelir view` MUST store audio (mp3, wav, ogg, oga, m4a, flac, opus, aac) and video (mp4, mkv, mov, webm, avi, m4v) up to 256 MB and queue an `attachment-note`, sending no bytes to the provider.
- **FR-035**: A prompt MUST carry at most 8 images. The prompt text holds a pointer (`[image: attachments/…, WxH, size]`), not bytes.
- **FR-036**: On refusal `kelir view` MUST exit 1, print a message and record nothing.

**Channel mode**

- **FR-037**: `kelir channel [--server NAME…] [--max-messages N] [--status-once]` MUST serve all `channel = true` servers (or the named ones) and exit 1 with a message if none is configured.
- **FR-038**: System MUST accept a channel notification in either bridge shape (`message_id`/`conversation_id`/`sender`/`content`, or `id`/`conversation`/`sender_name`/`text`, with `meta.chat`/`meta.sender` as fallbacks). A missing sender is `unknown`.
- **FR-039**: System MUST append every accepted inbound message to `spool/<server>.jsonl` with fsync before processing, with records `inbound`, `claimed`, `done` (and `compacted`), deduped by message id.
- **FR-040**: System MUST replay unfinished messages after a restart, repair a torn tail, and compact the spool above `[channel] spool_limit` keeping unfinished messages and one `done` per id, so a done id stays deduped.
- **FR-041**: System MUST map each conversation to one stable session name `chan-<server>-<conversation>` (safe alphabet, length-bounded, hash-suffixed so different ids never collide). A new conversation starts a session. A known one resumes it.
- **FR-042**: System MUST run one turn at a time per conversation and at most `[channel] max_active` (default 4) conversations at once.
- **FR-043**: System MUST rate-limit with a one-minute window: `per_conversation_rpm` (default 20) and `global_rpm` (default 120). A limited message stays in the spool and is retried later.
- **FR-044**: System MUST wrap the text in an envelope marking it untrusted and non-operator, run `prompt_submit` gates on the envelope, and record the `user` event with `from: channel`. Image attachments are saved into the session and pointed to by path.
- **FR-045**: System MUST answer with a single reply-tool call (`reply_tool`, default `reply`), then send `notifications/claude/channel/turn_done` (failure ignored), then mark the message `done`.
- **FR-046**: On a turn that failed, paused or timed out, System MUST send the fallback text (`fallback` default "Sorry — I couldn't finish that. Please try again."; a distinct default for timeouts and for gate denials) unless the config sets its own, which then applies to all.
- **FR-047**: System MUST map a first line starting with `/stop` to the `stop` verb (rest of the line as reason), authorised by the target session's own token, and never send it to the model.
- **FR-048**: System MUST report OSC 7501 `working` while any turn runs and `idle` otherwise, and write `spool/status.json` (conversations, queue depth, active turns, spool backlog per server, reply and fallback counts). `--status-once` MUST print one JSON status line at startup.

**Cross-platform process layer**

- **FR-049**: All child processes for gates, hooks, credential helpers and MCP servers MUST start through the one runner, with an allow-listed environment (including any `LC_*`), optional timeout, capped stdout (default 1 MiB) and stderr (8 KiB), no console window on Windows, and group kill.
- **FR-050**: System MUST write a long shell command (over 6,000 bytes on Unix, any length on Windows) to a script file named for the shell family (`-c` shells run the file, `/C` keeps its switch, `-Command` becomes `-File`). An unrecognised shell on Windows MUST be an error that explains the accepted switches.
- **FR-051**: System MUST provide an atomic file write (temp file beside the target, then rename) and an exclusive create that never overwrites someone else's file.
- **FR-052**: System MUST provide a single-flight job lock with token-guarded release (only the acquirer removes it), liveness by pid and start time, a 1 s freshness guard, a 30 s age before unreadable lock files are taken over, and a two-look takeover.
- **FR-053**: System MUST create the session token (256 bits of OS randomness, 64 hex characters) and refuse to start when the OS gives none. Job ids are `bg_<128 bits>` or `call_<128 bits>` minted per session, with bounded budgets for refused-verb logging (32), id claims (256) and acknowledgements (16).
- **FR-054**: System MUST install an interrupt listener once per process. A pending interrupt ends the current wait as `stopped`, and a second interrupt within 2 s exits with code 130.

### Key Entities

- **MCP server**: a configured stdio process with name, command, args, env, description, timeout, channel flag, reply tool and declared capabilities.
- **Broker**: the session-side owner of server connections, tool lists and call settlement.
- **Call**: one `tools/call` with an id, a log pair (`call_started`/`call_settled`), an outcome (`ok`, `error`, `timeout`, `unavailable`, `disabled`, `unknown`) and an exit code.
- **Source**: anything beyond the conversation that enters the context, with kind (prompt, instruction, skill, memory, herdr, hint, attachment, attachment-note, channel), name, path (or none), content hash and layer (machine or project).
- **Skill**: a directory with a `SKILL.md` (front matter `name`, `description`, optional `metadata`), pulled on demand. Three are built in.
- **Attachment**: a file copied into `<session>/attachments/` (an image, or an audio/video file with a note).
- **Channel message**: id, server, conversation, sender, text, optional `reply_to` and attachments.
- **Spool**: the append-only per-server record file (`inbound`, `claimed`, `done`).
- **Conversation**: one queue and a running flag, mapped to one named session.
- **Process group**: a Unix process group or Windows Job Object holding a child's tree.
- **Job lock / session token**: the single-flight record and the secret that authenticates socket callers.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A model can call any configured MCP tool with no schema in its request. 100% of schema-invalid argument sets are rejected locally with exit 2 and zero server traffic.
- **SC-002**: After a session ends or is SIGKILLed, zero MCP server processes or grandchildren remain.
- **SC-003**: A canary secret placed in a server's `env` appears in no log, error message, command line or call input record.
- **SC-004**: Every MCP call, including ones that time out or are cut off by a kill, has exactly one `call_started` and one `call_settled`.
- **SC-005**: Editing an instruction file, memory note or skill mid-session changes no byte of the earlier request prefix and produces exactly one `source` message per edit.
- **SC-006**: The embedded base prompt is at most 1,024 bytes.
- **SC-007**: A message already recorded `done` is never answered twice across any number of restarts, and an unfinished message is answered exactly once after a restart.
- **SC-008**: With 1,000 messages queued, no more than `max_active` turns ever run at once and no conversation has two concurrent turns.
- **SC-009**: A non-image or oversize file passed to `kelir view` leaves the log and the attachments folder unchanged.
- **SC-010**: A shell command of any length runs unchanged on Windows and Linux. No silent truncation occurs.
- **SC-011**: A test or run that spawns processes leaves none behind and opens no visible console window on Windows.

## Assumptions

- Operators trust the project they open: `.kelir/config.toml` can name any server `command`, and `channel = true` starts it at session start. A trust prompt for project servers is not implemented.
- Same-user processes are trusted. The token and owner-only permissions keep other users out, not other processes of the same user.
- A concrete chat bridge, such as a Telegram bot, is a separate MCP server supplied by the operator. kelir ships only the generic mechanism. The only Telegram-specific code is the `herdr-telegram` pane marker.
- The client is a hand-written JSON-RPC subset with no MCP SDK dependency.
- Audio transcription and video frame sampling are not implemented.
- Windows is a first-class platform. The Linux half is verified by the gate's `--linux` run.

## Code vs docs discrepancies

1. `src/features/channel/driver.rs:23` says a gate-denied prompt sends "no answer to the remote user". `src/features/channel/run.rs:320` sends `GATED_FALLBACK` ("That message was blocked by policy…") as the reply.
2. `src/features/channel/run.rs:368-370`: the status `replies` counter counts every reply sent, including fallback text, and `fallbacks` counts replies that failed to send. `docs/reference/channels.md` ("reply/fallback counts") reads as if `fallbacks` counted fallback replies.
3. `src/features/sources/source/discover.rs:190-195`: a skill directory with a missing or unreadable `SKILL.md` is skipped silently (`read_file` returns `None`, its notes are dropped). `docs/reference/sources-and-skills.md` (Skills) says missing, unreadable or malformed is skipped with an `error` note. Only a malformed one gets a note.
4. `src/sys/runner.rs:19-63`: the allow-list also passes `USER`, `LOGNAME`, `SHELL`, `TERM`, `TZ`, `XDG_*`, `SSL_CERT_DIR`, `ALL_PROXY` and `SSH_AUTH_SOCK` to MCP servers. `docs/reference/mcp.md` ("The server's environment is an allow-list") lists a shorter set and says "nothing else".
5. Docs name source files that no longer exist: `docs/reference/mcp.md:8` (`src/mcp/`), `docs/reference/channels.md:9` (`src/channel.rs`, `src/mcp/`), `docs/reference/sources-and-skills.md:7` (`src/sources.rs`, `src/attachments.rs`, `src/config.rs`). The code is under `src/features/…`.
6. The assignment names a Telegram channel. The code has no Telegram bridge. Only `src/features/herdr/mod.rs:55-77` knows `TERM_PROGRAM=herdr-telegram`. `docs/reference/channels.md` agrees that kelir ships a generic mechanism.

## Source map

- src/features/mcp/mod.rs
- src/features/mcp/config.rs
- src/features/mcp/args.rs
- src/features/mcp/cli.rs
- src/features/mcp/broker.rs
- src/features/mcp/rpc.rs
- src/features/mcp/render.rs
- src/features/mcp/feature.rs
- src/features/sources/feature.rs
- src/features/sources/source/discover.rs
- src/features/sources/source/render.rs
- src/features/sources/source/types.rs
- src/features/sources/source/watch.rs
- src/features/memory.rs
- src/features/attachments.rs
- src/core/session/view.rs
- src/features/channel/mod.rs
- src/features/channel/message.rs
- src/features/channel/driver.rs
- src/features/channel/run.rs
- src/features/herdr/mod.rs
- src/config/load.rs
- src/main_cli.rs
- src/sys/runner.rs
- src/sys/shell.rs
- src/sys/atomic.rs
- src/sys/joblock.rs
- src/sys/signals.rs
- src/sys/trust.rs
- src/sys/perm.rs
- src/sys/proc/
- src/prompt.md
- docs/reference/mcp.md
- docs/reference/sources-and-skills.md
- docs/reference/channels.md
- tests/it/mcp_core.rs
- tests/it/mcp_security.rs
- tests/it/channel_core.rs
- tests/it/channel_edges.rs
- tests/it/channel_restart.rs
- tests/it/sources_live.rs
