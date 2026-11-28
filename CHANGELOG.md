# Changelog

All notable changes to kelir are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
semantic versioning. The milestone-by-milestone evidence (test counts, commit
ids, reviewer verdicts) is in `docs/PROGRESS.md`.

## [Unreleased]

### Added

- **Word keys in the prompt editor**: `Ctrl-W` and `Ctrl-Backspace` delete a whole word (a pasted
  image or file path deletes as one chip, not as scattered characters), and `Alt-b` / `Alt-f` join
  `Ctrl-Left` / `Ctrl-Right` and `Alt-h` / `Alt-l` as word motion. Several spellings are bound
  because terminals differ in what they send for the same key.
- **`/new`**: ends the current session cleanly and starts a fresh one in the same directory, with
  the same model, provider and config, and no `--resume`. The finished session keeps its log and
  its resume line; the new process `exec`s the binary in place on Unix, so no parent is left
  holding the terminal.
- **Built-in `timebox` gate**: `[gates.timebox] duration = "30m"` makes a turn that says "you have
  30 minutes" keep working until the window closes. The gate fires at the end of a turn and denies
  with steering (`{elapsed}`, `{target}` and `{left}` are filled in), up to `max_denies` times
  (default 10), so an agent that wants to stop five minutes in is pushed to go deeper instead. A
  deny budget is now per gate, not one per turn: `max_denies = 1` is the old behaviour.
- `[providers.opencode-go] refresh_cmd`: when opencode's stored credential is expired or inside
  its two-minute margin, kelir runs this command once (a command that makes opencode refresh
  *itself*) and reads the store again, so a host that rarely uses opencode no longer needs a
  manual `opencode` run, an env copy or a second refresher (#61).

### Fixed

- Leaving the TUI gives the terminal back clean: the run used to paint its frames onto the
  caller's screen (raw mode only — the alternate buffer was never entered, though the teardown
  left it) and then printed the resume line underneath, so the finished conversation stayed in the
  shell and a resumed pane inherited it. The buffer is entered before the first frame, and the
  exit now clears the visible rows and prints nothing but `resume: kelir tui --resume <id>`. The
  person's own scrollback is left alone (`2J`, never `3J`).
- The TUI's mouse wheel now scrolls the transcript, three rows a notch, in both directions
  (reaching the bottom resumes following). It reports the wheel only — never motion — and
  releases the terminal on every exit path, so the pane keeps its own drag-selection and Herdr
  keeps the wheel outside `kelir tui`. `specs/005-tui` FR-003 said "no mouse capture"; the
  wheel is a documented deviation.
- A prompt submitted in the TUI is shown once. The local echo remembered its *rendered* row
  (`› hello there`) while the log carries the plain text (`hello there`), so the log copy
  never matched the echo and the prompt was appended a second time as a bare line.
- `/say-now` gets the same single row, and its `(now)` marker no longer duplicates it either.

### Breaking — the rewrite

- Event log v1, the state layout, the CLI, the config file and the wire format all
  change with the rewrite (#22). Sessions, logs and configs from `0.1.0` do not
  carry over: start fresh.

### Removed — the rewrite

- `[prices]`, `warn_cost`, and cost in `ls` / `status` / STATUS are gone: cost was
  display-only and never had a value in practice.
- `[hint]` is gone; use `[gates.hint] builtin = "hint"` instead.
- `jobs.toml` and `kelir job …` are gone; use
  `kelir run --name --single-flight --timeout --fresh`.
- `kelir docs --generate` is gone: the reference is served as written.
- Signed background status files and per-child links are gone; the per-session
  token and owner-only directories remain.

### Added — the rewrite

- Built-in gates, behind the same interface as external gates.
- The gate `context` answer.
- `run --single-flight`, `run --timeout` and `run --fresh`.

### Changed — the rewrite

- Every resumed session gets a date-change source: a source file edited on disk
  lands as a `source` event at the tail, without rewriting the frozen system
  prompt.
- Windows resolves `shell = "bash"` to a Git for Windows `bash.exe`, never the
  System32 WSL stub. The background shim gives the child writable stdout and
  stderr handles, so a command that prints to either stream no longer settles
  with exit 1 and no output. An external `kelir kill` signals a session that
  cannot answer its socket because a gate is still running: the fallback walks
  the registered process tree by pid and start time and reports failure if the
  process is still alive when the grace period ends, so a non-leader session
  is really killed and an unrelated process group is never signalled.

### Fixed — rewrite P6 live drive

- The hint scorer now sends the provider session header, so scoring uses the
  same session as the live request (#64, found by the Linux live drive).
- On a Windows PC, the background shim writes `bash` tool output to writable
  stdout/stderr handles, `bg` sessions settle correctly, and `kelir kill` during
  a running gate terminates a busy session without signalling an unrelated
  process group (#65, found by the mock-provider drive).

### Breaking — rewrite P4B CLI surface

- Three redundant subcommands are gone: `kelir ask` is `kelir run --ask`,
  `kelir resume <id> [task]` is `kelir run --resume <id> [task]`, and
  `kelir sessions ls|rename|forget` is `kelir ls sessions|rename|forget`. No
  alias or compatibility subcommand is kept. Scripts and the builtin skills use
  the new spellings.

### Breaking — rewrite P4B config and environment

- `config.toml` is grouped by feature: `[compaction]`, `[retry]`, `[children]`,
  `[channel]`, `[notify]` and `[mcp]` replace the flat `auto_compact_at`,
  `compact_at_tokens`, `compact_prompt`, `compact_keep`, `retry_*`,
  `channel_*`, `max_child_*`, `max_children`, `max_live_descendants` and
  notification-hook keys. Every typed table, including `[providers.<name>]` and
  `[mcp.servers.<name>]`, has `deny_unknown_fields`: an unread key is now an
  ordinary unknown-key error, with no tailored "was removed" message. `[notify]`
  is the one exception: its dynamic hook names are validated by the reader with
  the same ordinary error wording. A literal `key` is refused rather than
  warned about. Bad values are reported without echoing the offending value, so
  a secret in `env` never reaches the model or the log.
- MCP servers are configured under `[mcp.servers.<name>]` in `config.toml`.
  `mcp.toml` and `KELIR_MCP_CONFIG` are gone; the machine/project layer rule
  still applies. `--config` or `KELIR_CONFIG` selects the file used by both the
  session and its MCP broker; one-shot `kelir mcp` has no `--config` flag and
  reads `KELIR_CONFIG` or the machine/project search.
- The session socket's JSON envelopes (`Request`, `Response`) reject unknown
  fields; an extra key is an ordinary deserialisation error, not a silent
  no-op. The envelope is documented once, in `docs/reference/verbs.md`.
- Test-only switches are now namespaced: `KELIR_TEST_CRASH_AT`,
  `KELIR_TEST_NOTIFY_TIMEOUT_MS`, `KELIR_TEST_TODAY`,
  `KELIR_TEST_GATE_TIMEOUT_MS`, `KELIR_TEST_SHELL_RAW` and
  `KELIR_TEST_IPC_LOCK_PROBE`. No other `KELIR_*` variable was removed.
### Breaking — rewrite P4B config and environment
### Breaking — rewrite P4B state layout

- A background job now owns exactly one directory: session jobs live under
  `<session>/calls/<id>/` (status, output, markers), jobs started outside a session
  `<state>/calls/<id>/`. There is no second location, no dual-dir reader and no
  compatibility path; direct readers must use the new location.

- Tool and MCP call output directories moved from `<session>/commands/` to
  `<session>/calls/`. Direct readers of the old path must switch; existing sessions
  are not migrated (no compatibility readers).

- Global call artefacts follow the same one-directory-per-call rule: session-less
  background jobs moved from `<state>/bg/<id>/` to `<state>/calls/<id>/`, one-shot
  MCP output from `<state>/mcp/<id>/` to `<state>/calls/mcp_<id>/`. The channel
  status snapshot moved from `<state>/channel-status/status.json` to
  `<state>/spool/status.json`, beside the spool it describes. No old path is read.

### Breaking

- The TUI now owns the terminal's alternate screen instead of writing finished
  turns into the terminal scrollback. Prompts and answers stay in an in-app
  transcript reachable with PageUp/PageDown/Home/End; resize reflows it and
  keeps the reader's scroll position. `Ctrl-Shift-Y` writes the whole
  transcript to `transcript.txt` in the session directory. On exit the
  alternate screen, kitty keyboard handshake, bracketed paste and raw mode are
  restored; the user's own scrollback is untouched.

- The event log is format 1. `session_started` carries `format: 1` and a reader
  refuses a log written in another format ("written by a different kelir format;
  start a new session"); there is no compatibility reader. The 16-type vocabulary
  (down from 24): `usage` folds into `assistant.usage` (with `step_usage` for
  requests that have no assistant event, such as compaction and gate scoring);
  `tool_started`, `tool_settled`, `mcp_started`, `mcp_settled`, `bg_started`,
  `bg_settled`, `child_started` and `child_settled` become `call_started` and
  `call_settled` with `kind: tool|mcp|bg|child` and kind-specific fields under
  `info`; `warning` and `notify` become `note` with `note.kind` (`error` stays its own type); the
  write-only `resumed` event is gone. Consumers of `events.jsonl` must update.

- `kelir docs --find` is now `kelir docs --search`; it prints `topic:line: text` hits,
  best-matching topic first. `kelir docs --generate` and `kelir docs --check` are removed:
  `docs/reference/` is hand-maintained markdown and `cargo test --lib docs` is the one
  drift check (code names must appear in the reference). `KELIR_DOCS_ROOT` is no longer read.

- `[hint]` is gone; the pushed hint is now the built-in `hint` gate declared with
  `[gates.hint] builtin = "hint"` and a `model`. A config that still has `[hint]`
  gets the ordinary unread-key treatment.
- Gates may answer with `context` (and `kind`): built-in and external gates
  share the append path into a `source` event at the tail. A built-in's own
  provider spend is a `usage` event named after the gate (`kind: "hint"`).
  A failed built-in's note is an `error` event with `where: "gate"` (the old
  core hint used `where: "source"`).

### Added (milestones M0 – M11, in landed order)

- **M0** — the harness skeleton: the event log (`events.jsonl`), the `bash`
  tool with bounded output and spill to `blobs/`, the fold/read-back path, the
  first provider integration and the review harness.
- **M1** — process handling: Job Object assignment before spawn on Windows,
  tree kill on failure, and the no-visible-window guarantee.
- **M2** — model defaults and provider model metadata, model lists and retries.
- **M3 / M4** — IPC and session locking, named jobs and `bg` background calls,
  the concurrency governor, crash-injection tests and the shared gate lock.
- **M5-A / M5-B** — sources and skills (the harness reads its own `AGENTS.md`
  and skill directories), the `source` event, the `skills` verb; then
  improvements to shell command shaping and short-vs-long command parity.
- **M6** — TUI v2: a redrawn interactive session with facts, scrollback and
  countdown displays for retries and waits.
- **M7** — review channels, prompt-submit gates and the bridge protocol for
  driving sessions from another process.
- **M8** — MCP support: `mcp.toml`, `kelir mcp`, the session broker and channel
  servers, with an allow-list trust model.
- **M9** — two providers with credential-store reads, key-source logging,
  expiry handling and a per-provider concurrency cap.
- **M10** — TUI v3 and attach-viewer work, plus MCP and gate hardening.
- **M11** — attachments and notifications: file/attachment handling and
  notify hooks under the scrubbed environment.
- **STEER-9** — de-flaked the e2e suite; three consecutive loaded gates with
  zero retries.

### Breaking

- Cost tracking is gone: the `[prices]` table, `warn_cost`, the `cost` and
  `cost_source` fields of `usage` events, the cost column of `kelir ls` and
  `sessions ls`, cost in `status` and job STATUS, and the TUI's cost state.
  Token counts, `warn_prompt_tokens` and the cache-hit ratio stay. A config
  with `prices` or `warn_cost` gets the ordinary unread-key treatment (no
  special "removed" message); old logs are not read by the rewritten binary.

### Rewrite (issue #16)

- Cut `jobs.toml` and `kelir job run|status|history`; scheduled runs are
  `kelir run --name --single-flight` with `--timeout`, `--task-file` and
  `--fresh`. STATUS is one `names/<name>.status.json` per name, no history.
- Removed module-static run state: the deadline watcher and finalize path
  share a per-run `Arc<RunState>`.

### Changed

- The gate now caps itself at four build jobs and four test threads, takes a
  single machine-wide lock, and names any test that needed a retry.
- Crash-injection runs disable core dumps on every gate path.
- Repo hygiene: the published tree contains no private names, hosts or paths.

### Security

- Session directories and tokens are owner-only; key values are never logged,
  only their source; child environments are scrubbed before spawn.
- Signed bg status, per-job capabilities, per-child links and permission
  read-back are gone. A session token, owned-job checks and settle-once keep
  other users and duplicate reports out. `bg_started` no longer carries
  `pubkey`; status files are plain JSON.

## [0.1.0] — pending release

The first published version. It corresponds to the state of `main` at the
publication commit recorded in `docs/PUBLISH.md`. The changelog above is the
release note; the detailed milestone table is in `docs/PROGRESS.md`.
