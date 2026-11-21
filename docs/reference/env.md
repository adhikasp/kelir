# Environment variables

**Summary:** every variable kelir reads, every one it sets for its children, and
the test hooks it honours; the table is generated from `src/envs.rs`, which is
the only place a variable is declared.
**Read this when:** you are configuring a session without a config file,
debugging "why did it pick that directory / model / colour", or adding a knob
(`src/envs.rs`: `VARS`).

Only the two provider keys carry secrets, and they are only ever read: no code
path prints, logs or puts one on a command line. `kelir doctor` reports the
*source* (`env OPENROUTER_API_KEY`, `opencode auth export opencode`) and never
the value; `session_started.key_origin` records the same.

## The variables

<!-- generated:env -->
### Read by kelir

| variable | effect |
|---|---|
| `OPENROUTER_API_KEY` | the OpenRouter provider's key (never printed: `kelir doctor` reports the source) |
| `KELIR_GO_API_KEY` | the OpenCode Go key, an override for opencode's own store |
| `KELIR_CONFIG` | the config file to use instead of the machine/project search |
| `KELIR_CONFIG_DIR` | the machine config directory (`config.toml`, `prompt.md`, `skills/`) |
| `KELIR_MCP_CONFIG` | the machine `mcp.toml` to use instead of `<config dir>/mcp.toml` (the project's `.kelir/mcp.toml` still layers on top) |
| `KELIR_STATE_DIR` | the state root (sessions, model caches, bg jobs, locks) |
| `KELIR_HOME` | replaces `$HOME`/`USERPROFILE` only (user skills, `~`); it does NOT move state or config, set `KELIR_STATE_DIR` and `KELIR_CONFIG_DIR` for that |
| `KELIR_DOCS_ROOT` | the `docs/reference` directory `kelir docs --generate` and `--check` read (a checkout, or a copy) |
| `KELIR_BASE_URL` | overrides a provider's base URL (a proxy, or a test's mock server) |
| `KELIR_PROMOTE_AFTER` | seconds before a still-running bash call becomes a background job (overrides `promote_after`) |
| `KELIR_HERDR` | force Herdr status records on (`1`) or off (`0`) |
| `KELIR_ASCII` | non-empty: ASCII glyphs instead of Unicode in the TUI |
| `NO_COLOR` | non-empty (the convention): no colours; attributes and glyphs stay |
| `HC_TERMINAL_ID` | set by herdr-core in every pane: its presence makes kelir ask `hc get` once for the pane/space/tab line in the frozen system prompt |
| `TERM` | `dumb` picks the ASCII glyph set |
| `TERM_PROGRAM` | `herdr-telegram` marks a pane a Telegram bridge reads (a job reports `idle`, not `done`) |
| `HOME` | the home directory on Unix (`~`) |
| `USERPROFILE` | the home directory on Windows (`~`) |
| `XDG_STATE_HOME` | Linux state root when `KELIR_STATE_DIR` is unset |
| `XDG_CONFIG_HOME` | Linux config root when `KELIR_CONFIG_DIR` is unset |
| `XDG_RUNTIME_DIR` | the socket directory on Unix, when a session has no directory yet |
| `APPDATA` | Windows config root when `KELIR_CONFIG_DIR` is unset |
| `LOCALAPPDATA` | Windows state root when `KELIR_STATE_DIR` is unset |
| `PATH` | where `bash`, `rg`, `opencode` and other programs are looked for |
| `COMSPEC` | the shell that runs a `.cmd`/`.bat` helper (`opencode`) on Windows |
| `SHELL` | the login shell, when `bash` is not on `PATH` (Unix) |
| `KELIR_TRUST_PROJECT` | let this run start the project's own gates (`.kelir/config.toml` `[gates]`, project skills); the machine config's `trusted_projects` is the lasting form |

### Set by kelir for its children

| variable | effect |
|---|---|
| `KELIR_SOCK` | the session socket (a Unix socket path or a pipe name), so `kelir ctl`/`view`/`bg` reach the session |
| `KELIR_TOKEN` | the session's socket token: a tool child sends it with every request to the session |
| `KELIR_SESSION_DIR` | the session's directory |
| `KELIR_ROOT` | the root session of the tree this tool call runs in (its own id when it is the root) |
| `KELIR_PARENT` | the session this tool call runs in: the parent of any child it starts (`kelir run --child`) |
| `KELIR_DEPTH` | that session's depth in the tree (the root is 0); a start past `max_child_depth` is refused |
| `KELIR_CALL_ID` | the provider call id of the bash call |
| `KELIR_BIN` | the kelir binary, so `kelir bg`/`kelir view` work off `PATH` |
| `KELIR_GATE_EVENT` | set for a gate only: the event it was started for (`turn_end`, …); a gate never gets `KELIR_TOKEN`, `KELIR_SOCK` or a provider key |
| `KELIR_GATE_NAME` | set for a gate only: the gate's name, as the `gate` event records it |
| `KELIR_GATE_RUN` | set for a gate only: a token unique to this gate run, so kelir can find and kill what the gate left behind (a `setsid` daemon); do not clear it |
| `KELIR_JOB` | set for a job's notify hook only: the job's name |
| `KELIR_JOB_OUTCOME` | set for a notify hook only: `done`, `error`, `stopped`, `timeout` or `paused` |
| `KELIR_JOB_RUN` | set for a notify hook only: the run's id (`run_…`) |
| `KELIR_JOB_SESSION` | set for a notify hook only: the id of the session the run used |
| `KELIR_JOB_STATUS` | set for a notify hook only: the path of the job's `STATUS.json` |
| `KELIR_NOTIFY_EVENT` | set for an event hook only: the event that fired (`turn_done`, `error`, `paused`, `bg_settled`, `child_settled`) |
| `KELIR_NOTIFY_MESSAGE` | set for an event hook only: the terminal record's message (title and event kind) |

### Set by the test suite only

| variable | effect |
|---|---|
| `KELIR_NOTIFY_TIMEOUT_MS` | shorten the 30 s notify-hook timeout (the job tests) |
| `KELIR_TODAY` | `YYYY-MM-DD` replaces today's UTC date: a new session's system prompt and the date-change source of a resume (the job tests) |
| `KELIR_GATE_TIMEOUT_MS` | shorten the 30 s gate timeout (the gate tests; a timed-out gate fails open) |
| `KELIR_CRASH_AT` | abort at a named point (`before_append`, …): the crash tests |
| `KELIR_TEST_PANIC` | panic once the TUI owns the terminal, to prove the restore path |
| `KELIR_SHELL_RAW` | `cr`: send a POSIX script raw, keeping CRLF terminators |
| `KELIR_IPC_LOCK_PROBE` | a session dir: a test child tries to take the lock there, to prove a second process is refused (the IPC tests) |
<!-- /generated -->

## Notes

- A variable that is not in `src/envs.rs` cannot be read: `crate::envs::var` and
  `var_os` assert the name is declared, and a drift test fails when a
  `std::env::var` call appears anywhere else in `src/`. The one exception is a
  provider's `key_env` from the config, which names a variable kelir did not
  choose (`envs::named`).
- `KELIR_HOME` replaces only `$HOME`/`USERPROFILE` (user skills, `~`) so a run
  never reads the machine's own `~/.claude/skills`. It does not relocate state
  or config: `LOCALAPPDATA`/`APPDATA` (Windows) and `XDG_STATE_HOME`/
  `XDG_CONFIG_HOME` (Linux) still win. To isolate a run, set `KELIR_STATE_DIR`
  and `KELIR_CONFIG_DIR` as well.
- The two provider keys are read through `src/credentials.rs`, which also reads
  opencode's store. That module's precedence rules are in
  `docs/reference/providers.md`.
