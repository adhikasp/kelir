# Providers

**Summary:** exactly two providers — `openrouter` and `opencode-go` — with where
each key comes from, what is retried and for how long, the concurrency governor,
and where a model's context window, vision and effort support come from.
**Read this when:** a request failed, a key is "missing", a retry waits too long,
a model's window looks wrong, or you are adding a provider (`src/config.rs`,
`src/credentials.rs`, `src/provider.rs`, `src/retry.rs`, `src/models.rs`,
`src/governor.rs`).

There are exactly two, and a config naming a third is a usage error that says so.
`--provider` and `provider = "…"` take the same two names.

## Keys, in the order each provider tries

| provider | 1 | 2 | 3 |
|---|---|---|---|
| `openrouter` | `OPENROUTER_API_KEY` | `key_env` from the config, when it names another variable | `key_cmd` (a command's trimmed stdout) |
| `opencode-go` | `KELIR_GO_API_KEY` or a configured `key_env`/`key_cmd` (an *override*) | `opencode auth export opencode`, read directly | — |

The store read is the point: nobody has to copy a token into a
variable that goes stale. It is one `opencode` invocation, bounded at 5 s, and it
is repeated at most once — and only when the entry it returned is expired or
inside a two-minute margin — because running the CLI refreshes the token as a
side effect.

A host that rarely uses opencode can sit on an expired token, where re-reading
changes nothing. `refresh_cmd` (kelir#61) runs once when the entry is expired or
inside the margin (30 s bound, silent), then the store is re-read:
`["opencode", "run", "--archived", "-m", "opencode-go/beam-free", "ok"]` makes
opencode refresh itself, so the one-refresher rule holds.

What is recorded about a key is its **source**, never the value:
`session_started.key_origin` (`env OPENROUTER_API_KEY`, `key_cmd getkey`,
`opencode auth export opencode`), `kelir doctor`, and the one-line notice a
session prints. `key_cmd` is named by its file name, so a private path does not
reach the log. An expired store credential is a usage error (exit 2) with the
expiry and the fix ("run any `opencode` command once") *before* a request is
sent (after `refresh_cmd` has had its one chance, when configured).

## Retries

A retryable answer (429, 5xx, a reset connection) is retried with exponential
backoff: `[retry] base_ms` (2 s) doubling up to `cap_ms` (120 s), with jitter;
a provider's own `Retry-After` or reset header wins when it is under the cap and
above a floor. `[retry] max_wait` (30 min) is the total budget per request and
`[retry] max_attempts` (unset) an optional count cap. Running out **pauses** the
turn — `turn_ended` with `reason: "provider"`, exit code 0, and the log carries
one `error` event per attempt with `status`, `attempt` and `retry_in_ms`.

The wait is visible while it happens: the TUI's facts row counts it down, a dim
line goes into the scrollback, and `kelir run` prints it on stderr. The TUI word
is `rate limited`; the log's `error.message` is the status and its class
(`http 429: rate limited`), never a response body.

## Concurrency

`[providers.<name>] max_concurrent_requests` caps requests in flight *across every
local kelir process* through an OS file lock on `<state>/locks/<provider>-<slot>`:
a kelir that crashes leaks nothing, because the kernel releases the lock. A
session that has to wait for a slot shows the wait the same way it shows a
retry. `0` or unset is off.

## Model metadata

Nothing about a model is hard-coded: `GET {base}/models` answers it, both
provider shapes are read (OpenRouter's `context_length`/`supported_parameters`/
`architecture.input_modalities`, Go's `limit.context`/`capabilities`), the
normalised list is cached beside the state dir for a day, and the fetch runs
beside the session (a cold start gives it 250 ms; a slow answer is applied when
it lands).

- **Window**: `[model_windows]`, then `window`, then the provider's number, then
  `128000`. Which one won is recorded (`kelir` reports it at start).
- **Effort**: `reasoning_effort` is sent only where the list says it is supported,
  unless the config named it explicitly — an explicit choice is the person's, and
  a missing flag in the list is not a "no".
- **Vision**: a model the provider marks text-only refuses `kelir view`; a model
  the provider says nothing about is tried.
- **Parallel calls**: `parallel_tool_calls` is sent only to OpenRouter models
  that advertise it.

Each request's token counts arrive in the stream's `usage` chunk and are
recorded per step in the `usage` event, so a session's totals are the sum of its log.

## Adding a provider

There are two on purpose; a third is a product decision, not a config change.
The places a name reaches are `config::PROVIDERS`, `builtin_provider`, the
`ProviderKind` enum, `provider::build_request` (headers, cache control) and
`credentials::resolve_with` (the precedence above).
