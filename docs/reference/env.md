# Environment variables

**Summary:** every variable kelir reads, what it does, and which ones it sets
for its children. Only provider keys carry secrets, and they are only ever read.
**Read this when:** configuring a session without a config file, or debugging
"why did it pick that directory / model / colour".

## Read by kelir

| variable | effect |
|---|---|
| `OPENROUTER_API_KEY` | the OpenRouter provider's key |
| `KELIR_GO_API_KEY` | the OpenCode Go provider's key |
| `KELIR_CONFIG` | the config file to use instead of the search |
| `KELIR_CONFIG_DIR` | the machine config directory (`prompt.md`, `skills/`) |
| `KELIR_STATE_DIR` | the state root (sessions, model caches, locks) |
| `KELIR_HOME` | the base directory both of the above derive from (tests use it) |
| `KELIR_BASE_URL` | overrides a provider's base URL (tests, proxies) |
| `KELIR_PROMOTE_AFTER` | seconds before a call becomes a background job |
| `KELIR_HERDR` | force Herdr status records on (`1`) or off (`0`) |
| `KELIR_ASCII` | non-empty: ASCII glyphs instead of Unicode in the TUI |
| `NO_COLOR` | non-empty (the convention): no colours, attributes and glyphs stay |
| `TERM` | `dumb` implies the ASCII glyph set |
| `HC_TERMINAL_ID`, `HC_TERMINAL_SOCKET`, `HC_SCOPE` | set by Herdr; kelir reads the first to add one line about the pane and to report status |
| `XDG_STATE_HOME` | Linux state root when `KELIR_STATE_DIR` is unset |

## Set by kelir for its children

| variable | value |
|---|---|
| `KELIR_SOCK` | the session socket (a Unix socket path, or a pipe name) |
| `KELIR_SESSION_DIR` | the session's directory |
| `KELIR_CALL_ID` | the provider call id of the bash call |
| `KELIR_BIN` | the kelir binary, so `kelir bg`/`kelir view` work off `PATH` |

## Set by the test suite (never by kelir itself)

| variable | effect |
|---|---|
| `KELIR_TEST_PANIC` | panic right after the TUI owns the terminal (proves the restore path) |
| `KELIR_CRASH_AT` | die at a named point, for the crash tests |
