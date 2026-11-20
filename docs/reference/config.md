# `config.toml`

**Summary:** every key kelir reads from its config file, with the type, the
default, the flag or environment variable that overrides it, what it changes and
the function in `src/config.rs` that reads it.
**Read this when:** you are setting a session up on a new machine, a session did
something you did not ask for, or you are adding a key (`src/config.rs`:
`RawConfig`, `RawProvider`, `load`).

Two files merge, machine first, project second (so the project wins per key):

1. `KELIR_CONFIG` or `--config`, when set: that one file, and nothing else;
2. else `<config>/config.toml` (`%APPDATA%\kelir` on Windows, `$XDG_CONFIG_HOME/kelir`
   or `~/.config/kelir` on Linux, `~/Library/Application Support/kelir` on macOS),
   then the nearest `.kelir/config.toml` walking up from the cwd.

An unknown `[providers.<name>]` table is an error naming the two real provider
names. An unknown *key* is not: a config written for a newer kelir still loads.
A literal `key` (top level or in a provider table) is warned about and ignored —
keys come from the environment, `key_cmd`, or opencode's store.

## Keys

<!-- generated:config -->
### Top level

| key | type | default | override | effect | source |
|---|---|---|---|---|---|
| `provider` | string | `openrouter` | `--provider` | which of the two providers this session uses | `src/config.rs:load` |
| `model` | string | `deepseek/deepseek-v4.1-flash-1017` | `--model` | the model id sent with every request | `src/config.rs:load` |
| `shell` | string or array of strings | Windows: Git for Windows `bash.exe`, else `bash` on PATH, else `cmd.exe /C`; elsewhere: `bash` on PATH, else `$SHELL`, else `sh`, each with `-c`; the array is the argv prefix | — | the program `bash` tool calls and `kelir bg run` commands run in | `src/config.rs:load` |
| `spill_limit` | integer (bytes) | `10240` | — | output over this many bytes is written to `commands/<call>/out` and the model sees a spill notice instead of the whole text | `src/config.rs:load` |
| `max_steps` | integer | unset: no cap. `0` also means no cap | `--max-steps` | cap the steps of one turn; reaching it *pauses* the turn (`turn_ended` with `reason: "steps"`), and the next message starts a new turn | `src/config.rs:load` |
| `cache_ttl` | string | unset: the provider's own default | — | the `cache_control.ttl` sent with the request (`5m`, `1h`), for providers that price a longer break-even separately | `src/config.rs:load` |
| `prompt` | path | the embedded `src/prompt.md` | — | the base prompt file; when set it replaces `<config>/prompt.md` (read only when `prompt` is unset); only `<cwd>/.kelir/prompt.md` wins over it | `src/config.rs:load_prompt` |
| `providers` | table, keyed by provider name | both providers, with their built-in base URL and key source | — | per-provider settings (see the `providers.*` rows); any other table name is a config error | `src/config.rs:load` |
| `key` | string | — | — | **refused**: a literal key is warned about and ignored (the environment, `key_cmd` and opencode's store are the three sources) | `src/config.rs:load` |
| `window` | integer (tokens) | unset: the provider's model metadata, else `128000` | — | the context window for every model, for a provider that publishes none | `src/config.rs:load` |
| `model_windows` | table of integers (tokens), keyed by model id | empty | — | per-model window overrides; they win over `window` and over the provider's metadata | `src/config.rs:load` |
| `auto_compact_at` | float in `(0, 1]` | `0.8` | — | the fraction of the window at which compaction triggers automatically | `src/config.rs:load` |
| `compact_at_tokens` | integer (tokens) | `150000`; `0` disables the cap | — | an absolute soft target: compaction triggers at `min(window * auto_compact_at, compact_at_tokens)` | `src/config.rs:load` |
| `compact_prompt` | path | the embedded default in `src/compact.rs` | — | the summary prompt; `<config>/compact.md` is read when this is unset | `src/config.rs:load_compact_prompt` |
| `compact_keep` | integer | `4` | — | how many messages stay verbatim after a compaction | `src/config.rs:load` |
| `skills_dirs` | array of paths | empty (`~/.claude/skills`, `<config>/skills`, `.claude/skills`, `.kelir/skills` are always searched) | — | extra skills directories, named in the base prompt and searched read-only; `~/` and `$HOME/` expand | `src/config.rs:load` |
| `memory_dir` | path | unset: no memory | — | a directory of `.md`/`.txt` notes loaded as `memory` sources: rendered once into the frozen system, and a note edited or created later arrives as a `source` message at the tail; the model writes memory with `edit`; `~/` expands, a relative path is the session cwd's | `src/config.rs:load` |
| `hint` | table | unset: the pushed hint is off | — | `[hint]` with `model` (required to turn it on), `provider` (default: the session provider), `endpoint` (a base URL instead of the provider's; the provider's key is sent only when it has the same origin, else the call is keyless) and `timeout_ms` (default `2000`): one cheap scoring call per human prompt appends `might be relevant: name - description` as a `source` message of kind `hint`; it fails open | `src/config.rs:load` |
| `promote_after` | integer (seconds) | `600`; `0` disables promotion | `KELIR_PROMOTE_AFTER` | a bash call still running after this long becomes a background job instead of being killed | `src/config.rs:promote_after` |
| `retry_base_ms` | integer (ms) | `2000` | — | the first retry wait; each further attempt doubles up to `retry_cap_ms` | `src/config.rs:retry_policy` |
| `retry_cap_ms` | integer (ms) | `120000` | — | the longest a backoff grows to; it must be at least `retry_base_ms` | `src/config.rs:retry_policy` |
| `retry_max_wait` | integer (seconds) | `1800`; `0` waits as long as the provider asks | — | the total wait budget per request; running out pauses the turn (`reason: "provider"`) | `src/config.rs:retry_policy` |
| `retry_max_attempts` | integer | unset: only the budget and `retry_max_wait` bound the retries | — | a count cap inside the budget (`3` = three tries and out) | `src/config.rs:retry_policy` |
| `warn_prompt_tokens` | integer (tokens) | `150000`; `0` turns the warning off | — | when the last prompt is larger than this, one `warning` event and one dim line on                  the screen; it speaks again only after the prompt has fallen back under it.                  Never sent to the model | `src/config.rs:load` |
| `warn_cost` | number (US dollars) | unset: no cost warning | — | when the session's cost passes this, one `warning` event and one dim line; never                  sent to the model. Only costs that are known count | `src/config.rs:warn_cost` |
| `max_child_depth` | integer | `3` | — | the deepest a child may be (the root is depth 0); `kelir run --child` past it exits 3 with the count | `src/config.rs:load` |
| `max_children` | integer | `10` | — | live children one agent may have at once (a settled child frees its slot); the root's registry decides | `src/config.rs:load` |
| `max_live_descendants` | integer | `16` | — | live descendants one root may have at once, however deep; a child whose process is gone is reaped and frees its slot | `src/config.rs:load` |
| `prices` | table of tables `[prices."<model>"]` with `input`, `output`, `cached_input`                (dollars per million tokens) | empty: the built-in table (`src/spend.rs`, stamped with its date) applies | — | overrides and extends the built-in price table. Used only when the provider                  reports no cost; a model with no price anywhere gets no estimate                  (`cost_source: "unknown"`). `cached_input` defaults to `input` | `src/config.rs:prices` |

### Per provider (`[providers.<name>]`)

| key | type | default | override | effect | source |
|---|---|---|---|---|---|
| `base_url` | string | the provider's own endpoint | `KELIR_BASE_URL` (for the session's provider) | where requests go: a proxy, a gateway, or a test's mock server | `src/config.rs:load` |
| `key_env` | string | `OPENROUTER_API_KEY` / `KELIR_GO_API_KEY` | — | the variable the key is read from first | `src/config.rs:load` |
| `key_cmd` | array of strings | unset | — | a command whose trimmed stdout is the key, when the variable says nothing; only its file name is recorded | `src/config.rs:load` |
| `key` | string | — | — | **refused** here too: warned about and ignored | `src/config.rs:load` |
| `extra_headers` | table of strings | empty | — | headers added to every request to this provider | `src/config.rs:load` |
| `reasoning_field` | string | unset: no field | — | the request field a provider wants its effort in, when it is not the documented one | `src/config.rs:load` |
| `reasoning_effort` | string: `low`, `medium`, `high`, `none` | `medium` | — | how much hidden reasoning to ask for; `none` sends no field, and an explicit value reaches a model even when the provider's list says nothing | `src/config.rs:load` |
| `session_header` | string | `x-opencode-session` for `opencode-go` | — | the header carrying the session id, for a provider that requires one | `src/config.rs:load` |
| `max_concurrent_requests` | integer | unset: no limit | — | at most this many requests in flight to this provider across every local kelir process (`0`/unset = off) | `src/config.rs:load` |
<!-- /generated -->

## What the two provider tables are, exactly

`[providers.openrouter]` and `[providers.opencode-go]`, and nothing else: the
table name *is* the provider (`kind = "…"` is no longer read). Each one starts
from a built-in entry (its base URL, its key source, and its session header for
Go) and the file overlays what it names; `overlay()` in `src/config.rs` shows the
merge order.

## Where the prompt and the skills come from

- `prompt`: a file. When set it is used instead of `<config>/prompt.md` (that
  file is read only when `prompt` is unset); `<cwd>/.kelir/prompt.md` beats
  either. `sources[]` in `session_started` records which one
  won, so a log reader does not have to guess.
- `compact_prompt`: a file; `<config>/compact.md` is read when the key is unset,
  else the embedded default in `src/compact.rs`.
- `skills_dirs`: extra directories, named in the base prompt and searched
  read-only. `docs/reference/sources-and-skills.md` has the whole search order.
