# `config.toml`

**Summary:** every key kelir reads from its config file, grouped by feature, with the
type, default, override, effect and reader.
**Read this when:** setting a session up on a new machine, debugging a session that did
something unexpected, or adding a key (`src/config.rs`: `RawConfig` and the `Raw*` tables).

Two files merge, machine first, project second (the project wins per key):

1. `KELIR_CONFIG` or `--config`, when set: that file instead of the machine layer; the project layer still applies;
2. else `<config>/config.toml` (`%APPDATA%\kelir` on Windows, `$XDG_CONFIG_HOME/kelir`
   or `~/.config/kelir` on Linux, `~/Library/Application Support/kelir` on macOS),
   then the nearest `.kelir/config.toml` walking up from the cwd.

Every typed table has `deny_unknown_fields`: a typo is the ordinary unknown-key
error naming the field and its table. `[notify]` is the one exception: its hook
names are dynamic keys (`turn_done`, `error`, `paused`, `bg_settled`,
`child_settled`) captured in a flattened map, so serde cannot put
`deny_unknown_fields` on the table. The reader validates those names itself and
returns the same ordinary unknown-field wording. There are no tailored "this
was removed" messages and no compatibility readers: a config written for
another kelir format is rejected, not interpreted. A literal key is never read
— keys come from the environment, `key_cmd` or opencode's store.

The feature tables are `compaction`, `retry`, `children`, `channel`, `notify`
and `mcp`; their keys are nested, never flat aliases.

## Top level: one session

| key | type | default | override | effect | source |
|---|---|---|---|---|---|
| `provider` | string | `openrouter` | `--provider` | which of the two providers this session uses | `src/config.rs:load` |
| `model` | string | `deepseek/deepseek-v4.1-flash-1017` | `--model` | the model id sent with every request | `src/config.rs:load` |
| `shell` | string or array of strings | a Git for Windows `bash.exe` (installed path or a real Git bash on PATH), else `cmd.exe /C` on Windows; `bash` on PATH, else `$SHELL`, else `sh`, each with `-c`, elsewhere. A bare `shell = "bash"` on Windows resolves to a real Git bash, never the WSL stub. | — | argv prefix for `bash` tool calls and `kelir bg run` | `src/config/resolve.rs:resolve_shell` |
| `spill_limit` | integer (bytes) | `10240` | — | output above this is spilled to a file and the model sees a notice | `src/config.rs:load` |
| `max_steps` | integer | unset; `0` also means unlimited | `--max-steps` | cap the steps of one turn; reaching it pauses the turn | `src/config.rs:load` |
| `cache_ttl` | string | unset: provider default | — | `cache_control.ttl` (`5m`, `1h`) | `src/config.rs:load` |
| `prompt` | path | embedded `src/prompt.md` | — | base prompt; `<cwd>/.kelir/prompt.md` still wins | `src/config.rs:load_prompt` |
| `window` | integer (tokens) | provider metadata, else `128000` | — | context window when a provider publishes none | `src/config.rs:load` |
| `model_windows` | table of integers keyed by model id | empty | — | per-model window overrides | `src/config.rs:load` |
| `promote_after` | integer (seconds) | `600`; `0` disables | `KELIR_PROMOTE_AFTER` | promote a still-running bash call to a background job | `src/config.rs:promote_after` |
| `warn_prompt_tokens` | integer | `150000`; `0` turns it off | — | one `note{kind:"prompt_tokens"}` event and screen line when a prompt grows past it, never sent to the model | `src/config.rs:load` |
| `skills_dirs` | array of paths | empty (the default directories are always searched) | — | extra read-only skill directories | `src/config.rs:load` |
| `memory_dir` | path | unset | — | directory of notes rendered into the frozen system; later edits arrive as `source` messages | `src/config.rs:load` |
| `gates` | table | unset | — | external and built-in gates at the six named points; see `docs/reference/gates.md` | `src/config.rs:load` |
| `trusted_projects` | array of paths | empty | `KELIR_TRUST_PROJECT` | machine-only: projects whose own gates may run; a project layer's `gates` is ignored elsewhere | `src/config.rs:load` |
| `providers` | table keyed by provider name | both built-in providers | — | per-provider settings; only `openrouter` and `opencode-go` exist | `src/config.rs:load` |

## `[providers.<name>]`

| key | type | default | override | effect | source |
|---|---|---|---|---|---|
| `base_url` | string | provider's endpoint | `KELIR_BASE_URL` | proxy, gateway or mock server | `src/config.rs:load` |
| `key_env` | string | `OPENROUTER_API_KEY` / `KELIR_GO_API_KEY` | — | variable read first for the key | `src/config.rs:load` |
| `key_cmd` | array of strings | unset | — | command whose trimmed stdout is the key; only its file name is recorded | `src/config.rs:load` |
| `refresh_cmd` | array of strings | unset | — | only opencode's store: run once when the credential is expired or near expiry, then re-read it | `src/config/load.rs:load` |
| `extra_headers` | table of strings | empty | — | headers added to every request | `src/config.rs:load` |
| `reasoning_field` | string | provider default | — | request field for reasoning effort | `src/config.rs:load` |
| `reasoning_effort` | `low`, `medium`, `high`, `none` | `medium` | — | `none` sends no field; an explicit value overrides provider model metadata | `src/config.rs:load` |
| `session_header` | string | `x-opencode-session` for Go | — | header carrying the session id | `src/config.rs:load` |
| `max_concurrent_requests` | integer | unset: no limit | — | concurrency cap across local kelir processes | `src/config.rs:load` |

## `[compaction]`

| key | type | default | effect | source |
|---|---|---|---|---|
| `auto_at` | float in `(0, 1]` | `0.8` | fraction of the window that triggers compaction | `src/config.rs:load` |
| `at_tokens` | integer | `150000`; `0` disables | absolute soft target | `src/config.rs:load` |
| `prompt` | path | embedded default in `src/compact.rs` | summary prompt; `<config>/compact.md` is read when unset | `src/config.rs:load_compact_prompt` |
| `keep` | integer | `4` | messages kept verbatim | `src/config.rs:load` |

## `[retry]`

| key | type | default | effect | source |
|---|---|---|---|---|
| `base_ms` | integer | `2000` | first backoff | `src/config.rs:retry_policy` |
| `cap_ms` | integer | `120000` | backoff ceiling; must be at least `base_ms` | `src/config.rs:retry_policy` |
| `max_wait` | integer (seconds) | `1800`; `0` = unlimited | per-request wait budget | `src/config.rs:retry_policy` |
| `max_attempts` | integer | unset | optional attempt cap inside the budget | `src/config.rs:retry_policy` |

## `[children]`

| key | type | default | effect | source |
|---|---|---|---|---|
| `max_depth` | integer | `3` | deepest child (root is 0) | `src/config.rs:load` |
| `max_per_agent` | integer | `10` | live children per agent | `src/config.rs:load` |
| `max_live_descendants` | integer | `16` | live descendants per root | `src/config.rs:load` |

## `[channel]`

| key | type | default | effect | source |
|---|---|---|---|---|
| `max_active` | integer | `4` | conversations answered at once | `src/config.rs:load` |
| `per_conversation_rpm` | integer | `20` | messages per conversation per minute | `src/config.rs:load` |
| `global_rpm` | integer | `120` | messages across all conversations per minute | `src/config.rs:load` |
| `fallback` | string | `Sorry — I couldn't finish that. Please try again.` | reply when a channel turn fails | `src/config.rs:load` |
| `spool_limit` | integer (bytes) | `8388608` | compact the spool above this size | `src/config.rs:load` |

## `[notify]`

| key | type | default | effect | source |
|---|---|---|---|---|
| `hooks` | table of command strings or lists | empty | `turn_done`, `error`, `paused`, `bg_settled`, `child_settled`: scrubbed shell hooks, 30 s timeout, rate-limited | `src/config.rs:notify` |
| `terminal` | boolean | `false` | emit an OSC terminal record on a TTY | `src/config.rs:notify` |
| `terminal_kind` | string | empty | `osc9` or `osc777` | `src/config.rs:notify` |
| `rate_limit` | integer (seconds) | `0` | minimum interval between terminal records | `src/config.rs:notify` |
| `title` | string | empty | title used by the terminal record | `src/config.rs:notify` |

## `[mcp.servers.<name>]`

MCP servers used to live in a second file; they are now part of `config.toml`.
The two layers merge by server name: a project's `[mcp.servers.x]` replaces the
machine's whole, never field by field.

| key | type | default | effect | source |
|---|---|---|---|---|
| `servers` | table | empty | every MCP server, keyed by name | `src/config.rs:load` |
| `command` | string | required | program to start | `src/mcp/config.rs:from_raw` |
| `args` | array of strings | empty | argv after the command | `src/mcp/config.rs:from_raw` |
| `env` | table of strings | empty | server-only environment; never printed | `src/mcp/config.rs:from_raw` |
| `description` | string | empty | first line shown by `kelir mcp ls` | `src/mcp/config.rs:from_raw` |
| `channel` | boolean | `false` | connect when the session starts | `src/mcp/config.rs:from_raw` |
| `timeout` | integer (seconds) | `60`; `0` is refused | time allowed for one call | `src/mcp/config.rs:from_raw` |
| `reply_tool` | string | channel default | tool a channel server sends answers with | `src/mcp/config.rs:from_raw` |
| `declare_capabilities` | array of strings | empty | capabilities announced at `initialize`; permission relays are refused | `src/mcp/config.rs:from_raw` |

A name is letters, digits, `-` or `_`, and not `ls`. A malformed value is
reported with its table and field but never its value: an error reaches the
model and the log, and a server's `env` may hold a secret.
