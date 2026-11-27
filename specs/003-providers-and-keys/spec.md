# Feature Specification: Providers, Keys and Configuration

**Feature Branch**: `003-providers-and-keys`

**Created**: 2026-11-27

**Status**: Reverse-engineered from main e1cd8ea

**Input**: User description: "Reverse-build the spec of how kelir talks to its two model providers: configuration layers, where a key comes from and how it stays fresh, retry and pause on transient failure, the cross-process concurrency governor, usage accounting, cache-stable requests, model metadata, and `kelir doctor`."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - A session starts with a key kelir finds on its own (Priority: P1)

A person runs `kelir` against `openrouter` or `opencode-go`. kelir finds the key from the environment, a configured command, or (for OpenCode Go) opencode's own credential store, without the person pasting a token into a variable that goes stale. The key is held in memory only; only its *source* is ever recorded.

**Why this priority**: No request can be sent without a key; a wrong or leaked key is the worst failure of the area.

**Independent Test**: Start `kelir run --json` against a mock server with `OPENROUTER_API_KEY` set, then with only `key_cmd`, then (Go) with only a fake `opencode` on `PATH`; check the request's `Authorization` header and `session_started.key_origin`.

**Acceptance Scenarios**:

1. **Given** `OPENROUTER_API_KEY`, a different `key_env` variable and a `key_cmd` are all set, **When** a session starts on `openrouter`, **Then** the default variable wins, and `session_started.key_origin` reads `env OPENROUTER_API_KEY`.
2. **Given** only a `key_cmd` is configured for `openrouter`, **When** a session starts, **Then** the command's trimmed stdout is the key and `key_origin` reads `key_cmd <file name>` (the program's file name only, never its directory).
3. **Given** `opencode-go` with no `KELIR_GO_API_KEY`, no `key_env` value and no `key_cmd`, **When** a session starts, **Then** kelir runs `opencode auth export opencode`, uses the active `opencode` entry's `value.access` token, and `key_origin` reads `opencode auth export opencode`.
4. **Given** `KELIR_GO_API_KEY` is set, **When** a session starts on `opencode-go`, **Then** opencode is never run and `key_origin` reads `env KELIR_GO_API_KEY`.
5. **Given** no source answers for `openrouter`, **When** a session starts, **Then** it exits 2 with a usage error naming `OPENROUTER_API_KEY`, `key_env` and `key_cmd`, and no request is sent.

---

### User Story 2 - A rate-limited or flaky provider is ridden out, not fatal (Priority: P1)

A fully autonomous agent keeps working through a `429`, a `5xx` or a dropped connection. It waits visibly, can be interrupted, and when the waiting budget runs out the turn is paused, not failed.

**Why this priority**: Several local agents share one account; transient provider trouble is routine and must not lose a turn.

**Independent Test**: Script a mock server to answer `429` with `Retry-After`, then success; assert two waits in `events.jsonl`, then the step completing. Script endless `429`s with a small `[retry] max_wait`; assert `turn_ended` with `reason: "provider"` and exit 0.

**Acceptance Scenarios**:

1. **Given** a `429` with `Retry-After: 2`, **When** the request is retried, **Then** an `error` event (`location: "provider"`, `status`, `attempt`, `retry_in_ms`) is logged per attempt and the next request is sent after about that wait.
2. **Given** a `429` with no header, **When** retried, **Then** the wait is drawn with full jitter from `[0, base_ms * 2^n]`, capped at `cap_ms`.
3. **Given** the total wait budget (`max_wait`, default 30 min) is spent, **When** the next retryable failure arrives, **Then** the turn ends `paused` with `reason: "provider"`, exit code 0, and the next message starts a fresh attempt from the same conversation.
4. **Given** a wait in progress, **When** the person sends `stop` or `end`, **Then** the turn ends at once; **When** they send `model` or `say --now`, **Then** a new request is sent immediately with a fresh budget (on the new model for `model`).
5. **Given** a `400`, `401`, `403` or `404`, **When** it arrives, **Then** it is not retried as an outage.
6. **Given** a stream that dies before any tool call of the step began, **Then** the step is retried; **Given** it dies after a tool call began, **Then** it is not re-sent.

---

### User Story 3 - A stale credential heals itself (Priority: P2)

An OpenCode Go credential that has expired, is about to expire, or is rejected with 401/403 is re-read (and, if configured, refreshed through opencode itself) instead of failing the session.

**Why this priority**: opencode owns the refresh grant; a host that rarely runs opencode must still work, without kelir becoming a second refresher.

**Independent Test**: Fake `opencode` whose first export is expired and second is fresh: session start succeeds. Fake whose export stays expired: exit 2 naming the expiry and fix.

**Acceptance Scenarios**:

1. **Given** the store entry is expired or within two minutes of expiry, **When** a session starts, **Then** kelir re-reads the store once (after running `refresh_cmd` once, when configured) and uses the fresh entry if its expiry differs and is outside the margin.
2. **Given** the entry is still expired after that, **When** a session starts, **Then** it exits 2 with the expiry time and the fix ("run any `opencode` command once so it refreshes its credential, then retry"), before any request is sent.
3. **Given** `refresh_cmd` fails, **When** the entry stays expired, **Then** the error names the command's program (never its output) and says to fix it or run `opencode auth login`.
4. **Given** a live key within two minutes of its expiry, **When** the next step's request is about to be built, **Then** the key is re-read; if it changed, the client is rebuilt and an `error` event records the new expiry (not the key).
5. **Given** a `401` or `403`, **When** it arrives, **Then** the source is re-read once for that request; a changed key is retried once; an unchanged key means the provider's refusal is the answer.

---

### User Story 4 - Config layers merge, and a wrong config says so (Priority: P2)

A person writes a machine `config.toml` and a project `.kelir/config.toml`; the project wins per key. A typo or a third provider is a clear usage error.

**Why this priority**: Every other area reads configuration; silent fallbacks would hide mistakes.

**Independent Test**: Load configs with an unknown key, a third provider name, `cap_ms < base_ms`, and an invalid `reasoning_effort`; each fails with an ordinary error and nothing is sent.

**Acceptance Scenarios**:

1. **Given** a machine file and a project file, **When** config loads, **Then** tables merge deeply and the project's value wins per key.
2. **Given** `KELIR_CONFIG` or `--config` names a file, **When** config loads, **Then** that file replaces the machine layer and the project layer still applies.
3. **Given** `provider = "other"`, `--provider other` or a `[providers.other]` table, **When** config loads, **Then** it is a usage error naming `openrouter` and `opencode-go`.
4. **Given** an unknown key (including a literal `key = "…"` secret), **When** config loads, **Then** the error is the ordinary "unknown field" error and never quotes the value.
5. **Given** `KELIR_BASE_URL`, **When** a provider is resolved, **Then** its base URL is replaced (a proxy or mock server).

---

### User Story 5 - Concurrency is capped across local kelir processes (Priority: P3)

With `max_concurrent_requests = N` for a provider, no more than N provider requests are in flight across all local kelir processes; extra requests wait for a slot.

**Why this priority**: Opt-in protection against a self-inflicted 429 wave; off by default.

**Independent Test**: Two kelir processes with N=1 against a slow mock; assert the mock never sees overlapping requests. Kill the holder; assert the slot frees.

**Acceptance Scenarios**:

1. **Given** N=1 and one process holding the slot, **When** a second process needs to send, **Then** it shows a visible "waiting for a provider slot" wait and sends once the slot frees.
2. **Given** a holder is killed, **When** another process tries, **Then** it takes the slot (the OS releases the lock).
3. **Given** the knob is unset or `0`, **Then** requests are not serialised.
4. **Given** a `model` swap to the other provider, **Then** requests use that provider's slots.

---

### User Story 6 - `kelir doctor` shows where each key comes from (Priority: P3)

A person runs `kelir doctor [--json] [--provider <name>]` to see which source answers for each provider, the key's expiry and the read time, and the fix when there is none, with no key printed.

**Why this priority**: The first debugging step for "key missing" and "credential expired".

**Independent Test**: Run `doctor --json` with a fake opencode on `PATH`; assert key sources and absence of the secret in stdout/stderr.

**Acceptance Scenarios**:

1. **Given** both providers resolve, **When** `doctor --json` runs, **Then** one JSON object (`type: "doctor"`, `ok: true`, `providers` in order `openrouter`, `opencode-go`) is printed, exit 0.
2. **Given** the session's provider (default) has no key, **When** `doctor` runs, **Then** exit is 1.
3. **Given** the non-default provider has no key, **When** `doctor` runs, **Then** it prints `FAIL <name>` as information but exits 0.
4. **Given** the store credential is expired, **When** `doctor` runs, **Then** exit is 1 and the fix is printed.

---

### User Story 7 - Requests stay cache-stable and usage is measured (Priority: P3)

Each request carries the session identity the provider needs for sticky routing and prompt caching, and every response's token and cache counts are recorded so cache hits are measured, not assumed.

**Why this priority**: Cost and speed depend on cache hits; the proof is in the log.

**Independent Test**: Run two steps against a mock; compare request bodies/headers; read `usage` events.

**Acceptance Scenarios**:

1. **Given** `openrouter`, **When** a request is built, **Then** the body has `session_id`, `stream: true`, `stream_options.include_usage: true`, and never `provider.order`.
2. **Given** `opencode-go`, **When** a request is built, **Then** headers carry `x-opencode-session: <session id>` and `User-Agent: kelir/<version>`.
3. **Given** an Anthropic/Claude/Qwen model on OpenRouter, **Then** the body has top-level `cache_control: {"type":"ephemeral"}` (plus `ttl` if `cache_ttl` is set); other models get none.
4. **Given** a turn's usage chunk, **Then** `prompt_tokens`, `completion_tokens`, `cached_tokens` and `cache_write_tokens` are recorded per step, and a session's totals are the sum of its log.

---

### User Story 8 - Model facts come from the provider, not a table (Priority: P3)

kelir learns each model's context window, vision, tool, parallel-call and reasoning-effort support from the provider's `GET {base}/models`, caches it for a day, and never blocks the first request on it.

**Why this priority**: Drives compaction timing and which request fields are sent.

**Independent Test**: Serve `/models` from the mock in both provider shapes; check the resolved window and its recorded source; check `reasoning_effort` is omitted where unsupported.

**Acceptance Scenarios**:

1. **Given** `[model_windows]` names the model, **Then** that value is the window with source `config`; else `window`; else the provider's number (`api` fresh, `cache` from disk); else 128000 (`default`).
2. **Given** the list says a model does not take `reasoning_effort`, **Then** it is not sent unless the config set it explicitly for an unlisted model.
3. **Given** the provider marks a model text-only, **Then** `kelir view` refuses; **Given** it says nothing, **Then** images are tried.
4. **Given** a cold start, **Then** the fetch gets 250 ms before the first request; a slow answer is applied when it lands.

---

### Edge Cases

- A `key_cmd` printing nothing, failing, hanging (past 5 s) or flooding stdout (past 64 MiB): a usage error naming the program, never the output or secret.
- `Retry-After: 0` (or a reset header already past) forever: the wait never drops below a quarter second (or the base if smaller), so there is no hot loop.
- An absurd `Retry-After`: clamped to 7 days, then the budget decides: a wait that does not fit pauses the turn instead of hanging.
- The provider's error body echoes the key: non-retryable bodies are redacted and truncated to 500 characters; retryable errors carry only status and class.
- A 400 that names `reasoning_effort` or `parallel_tool_calls`: the field is dropped, the request is re-sent once at once, and the field is left out for the rest of the session. A second 400 is the answer.
- Two active `opencode` store entries, a JSON array without an active `opencode` entry, non-JSON output or a missing `opencode` program: clear usage errors.
- A resumed one-shot turn that failed on a retryable error is tried again up to 3 turn attempts.
- A summary (compaction) request is tried once and never waits through retries.
- An unreachable or failing `/models` fetch is retried at most hourly and reported in a few words (`timeout`, `offline`, `http 401`, `malformed`); the session proceeds on the default window.
- `[retry] max_attempts` is capped by an internal safety limit of 60 requests per budget.

## Requirements *(mandatory)*

### Functional Requirements

**Providers and configuration**

- **FR-001**: System MUST support exactly two providers, `openrouter` and `opencode-go`; `provider`, `--provider` and any `[providers.<name>]` table naming another MUST be a usage error that names both.
- **FR-002**: System MUST merge config as machine layer then nearest `.kelir/config.toml` walking up from the cwd, project winning per key; `KELIR_CONFIG`/`--config` replaces only the machine layer.
- **FR-003**: System MUST reject unknown keys in every typed table with an "unknown field" error that never quotes the value; `[notify]` hook names MUST be validated by the reader with the same wording.
- **FR-004**: System MUST ignore (with a warning) `trusted_projects` in a project layer, and keep a project layer's `gates` out of the machine merge.
- **FR-005**: System MUST default to provider `openrouter` and model `deepseek/deepseek-v4.1-flash-1017`, with base URLs `https://openrouter.ai/api/v1` and `https://opencode.ai/inference/go/v1`.
- **FR-006**: System MUST let `KELIR_BASE_URL` replace the base URL of whichever provider is resolved.
- **FR-007**: System MUST accept `reasoning_effort` of `low`, `medium`, `high` or `none` (case-insensitive); unset defaults to `medium` (non-explicit); `none` sends no field; anything else is a config error.
- **FR-008**: System MUST treat `max_concurrent_requests = 0` as unset, `max_attempts = 0` as unset, `max_wait = 0` as unlimited, and refuse `cap_ms < base_ms`.
- **FR-009**: System MUST refuse `refresh_cmd` on the `openrouter` table.
- **FR-010**: System MUST refuse `compaction.auto_at` outside `(0, 1]` and a non-numeric `KELIR_PROMOTE_AFTER`.

**Keys**

- **FR-011**: For `openrouter`, system MUST try `OPENROUTER_API_KEY`, then the variable named by `key_env` (if different), then `key_cmd`, first non-empty (trimmed) value winning.
- **FR-012**: For `opencode-go`, system MUST try `KELIR_GO_API_KEY`, then `key_env`, then `key_cmd`, and otherwise read opencode's store via `opencode auth export opencode` (the active entry whose `integrationID` is `opencode`; token `value.access`, falling back to `value.key`; `value.expires` in epoch ms, number or numeric string).
- **FR-013**: System MUST bound a key helper command to 5 s, run it with no stdin, never echo its output, kill the whole process tree on overrun, and stop reading past 64 MiB.
- **FR-014**: System MUST give a helper no provider key or session token in its environment.
- **FR-015**: System MUST record only a key's source (`env NAME`, `key_cmd <file name>`, `opencode auth export opencode`) in `session_started.key_origin`, `doctor` and errors; a key MUST never appear in the log, status, `observe`, error messages, the command line or a `Debug` print.
- **FR-016**: System MUST resolve the key once at session start (a failure is exit 2 before any request) and again for a swap to the other provider.
- **FR-017**: When the store entry is expired or within two minutes of expiry, system MUST re-read the store at most once (after running `refresh_cmd` once, 30 s bound, when configured), adopting the new entry only when its expiry differs and is outside the margin.
- **FR-018**: An expired credential MUST fail with the expiry time and a fix naming `opencode`, or the `refresh_cmd` program when one ran or failed.
- **FR-019**: System MUST re-read a key before building a request when it is within two minutes of expiry, and re-read it once per request after a 401/403, retrying only if the key changed; each re-read that changes the key MUST log an `error` event naming the source and new expiry.

**Retry and pause**

- **FR-020**: System MUST treat 429, 408 and every 5xx as retryable, and transport failures as retryable unless a tool call of the step had begun; other statuses MUST NOT be retried.
- **FR-021**: System MUST wait per attempt by full-jitter exponential backoff (`base_ms` 2000, doubling, capped at `cap_ms` 120000), unless the response's hint wins: `Retry-After` (seconds or HTTP-date), else the shortest non-zero reset header (`ratelimit-reset`, `x-ratelimit-reset*`, `anthropic-ratelimit-*-reset[-at]`); a zero `Retry-After` yields to a real reset window; hints are floored at 250 ms and clamped to 7 days.
- **FR-022**: System MUST keep a per-request waiting budget (`max_wait` default 1800 s) and optional `max_attempts`; a wait that does not fit MUST pause the turn (`turn_ended` outcome `paused`, `reason: "provider"`, exit 0) rather than be taken.
- **FR-023**: System MUST write one `error` event per failed attempt (`location: "provider"`, `retryable`, `status`, `attempt`, `retry_in_ms`) whose message is the status and class only for retryable failures.
- **FR-024**: System MUST show each wait (TUI facts row countdown and scrollback line; stderr in `kelir run` text mode) as `<reason> (<status>): retrying in <n>, attempt <k>, say stop to cancel`.
- **FR-025**: System MUST make a wait interruptible: `stop`/`end` end the turn; `model`/`say --now` apply and re-send at once with a fresh budget; other verbs keep their boundary.
- **FR-026**: System MUST NOT retry a summary request, and MUST NOT re-send a step whose stream had begun a tool call.
- **FR-027**: System MUST, on a 400 naming `reasoning_effort` or `parallel_tool_calls`, drop that field once per request, re-send, and omit it for the rest of the session.
- **FR-028**: A resumed one-shot turn failing with a retryable error MUST be re-attempted, up to 3 attempts in total.

**Governor**

- **FR-029**: With `max_concurrent_requests = N`, system MUST allow at most N requests in flight across local kelir processes, using OS file locks under `<state>/locks/`, and a request MUST wait visibly and interruptibly for a slot.
- **FR-030**: System MUST release a slot when the request's answer is in, and when its holder dies.
- **FR-031**: System MUST share a limit between provider configs that point at one endpoint, and follow a `model` swap to the new provider's slots.

**Request shape, cache and usage**

- **FR-032**: System MUST send OpenAI Chat Completions requests with `stream: true` and `stream_options.include_usage: true`, headers `content-type`, `user-agent: kelir/<version>`, `authorization: Bearer <key>`, plus configured `extra_headers` (lower-cased).
- **FR-033**: For `openrouter`, system MUST send `session_id` in the body, never `provider.order`; for `opencode-go`, `x-opencode-session: <session id>`; a `session_header` MAY name another header.
- **FR-034**: System MUST add top-level `cache_control: {"type":"ephemeral"}` (with `ttl` when `cache_ttl` is set) on OpenRouter only for model ids containing `anthropic/`, `claude` or `qwen`.
- **FR-035**: System MUST send `tools` only when non-empty, `parallel_tool_calls: true` only to OpenRouter and only when the model supports it, and `reasoning_effort` only when permitted (FR-040).
- **FR-036**: System MUST record per request `prompt_tokens`, `completion_tokens`, `cached_tokens`, `cache_write_tokens` and (Go) `allowance`; session totals are the sum of the log; a prompt over `warn_prompt_tokens` (default 150000, `0` off) warns once per crossing and is never sent to the model.
- **FR-037**: A non-retryable error message MUST have the key redacted and the body truncated to 500 characters.

**Model metadata**

- **FR-038**: System MUST read `GET {base}/models` in both provider shapes, cache the normalised list per endpoint for 24 h, retry a failed fetch at most hourly, bound the fetch to 30 s, and wait at most 250 ms for it at a cold start.
- **FR-039**: System MUST resolve the context window in the order `[model_windows]`, `window`, provider value (`api` or `cache`), 128000 (`default`) and record which answered.
- **FR-040**: System MUST send `reasoning_effort` only for a listed model that supports it, or an unlisted one when the config set it explicitly; a listed model without a flag is treated as supporting it.
- **FR-041**: System MUST refuse `kelir view` on a model the provider marks text-only and try one it says nothing about.

**Doctor**

- **FR-042**: `kelir doctor [--json] [--provider <name>]` MUST print each provider's base URL, key source, expiry, read time and fix, never a key; it MUST send no request to the provider, though it resolves keys exactly as a session does.
- **FR-043**: `doctor` MUST exit 1 only when the session's provider has no usable key or a credential is expired, exit 2 for a bad config or provider name, else 0.

### Key Entities

- **Provider**: one of `openrouter`, `opencode-go`; base URL, key source settings, header and reasoning settings, concurrency cap.
- **Credential**: a key held in memory with its source description and optional expiry; never serialised.
- **Retry policy / budget**: schedule (`base_ms`, `cap_ms`) and the per-request wait total and attempt count.
- **Slot**: one held concurrency lock for a provider endpoint.
- **Model info**: per-model window, tool, vision, parallel-call and effort support, cached per endpoint.
- **Usage record**: per-request token and cache counts in the log.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: No key value appears in any file under a session's state or in stdout/stderr across all key sources, including when a provider echoes it back.
- **SC-002**: A session on a host with a valid opencode login starts with no key variable set, and with an expired token plus a working `refresh_cmd`, it starts without manual action.
- **SC-003**: A provider outage shorter than the wait budget never fails a turn; a longer one pauses with exit 0 and resumes on the next message.
- **SC-004**: With N=1, two local processes never overlap a request; a killed holder frees its slot at once.
- **SC-005**: From the second step on, `cached_tokens` is reported in the log wherever the provider reports it.
- **SC-006**: `kelir doctor` completes offline within the 5 s helper bound per provider (plus the 30 s `refresh_cmd` bound when configured).

## Assumptions

- The user has `opencode` on `PATH` when relying on the OpenCode Go store.
- Provider responses follow OpenAI-style SSE and `usage` chunks.
- Child sessions and tool children reuse this area's key resolution; their policy lives in other specs.
- Compaction, gates (the `hint` gate) and children call the same request builder and resolution; their behaviour is out of scope here.

## Code vs docs discrepancies

- `docs/reference/providers.md:7-9,91`, `docs/reference/config.md:39+` (source column), `docs/reference/env.md:89` cite `src/config.rs`, `src/credentials.rs`, `src/provider.rs`, `src/retry.rs`, `src/models.rs`, `src/governor.rs`; the code is in `src/config/`, `src/provider/keys.rs`, `src/provider/{mod,retry,models,governor}.rs`.
- `docs/reference/providers.md:35` ("the one-line notice a session prints"): no code prints a key-source notice at session start; the source appears only in `session_started.key_origin` and `doctor` (`src/core/session/boot.rs:436,499`).
- `docs/reference/providers.md:59` says the lock file is `<state>/locks/<provider>-<slot>`; the governor is named by the base URL with `://` stripped, sanitised (`src/core/session/boot.rs:530-537`, `src/provider/governor.rs:61-66`).
- `docs/reference/providers.md:80` says `parallel_tool_calls` goes only to models that advertise it; for a model absent from the list, a known family name (`src/provider/models.rs:376-409`) also qualifies.
- `docs/reference/config.md:39` describes `window` as used "when a provider publishes none"; the code applies `window` before the provider's number (`src/config/resolve.rs:178`, `src/provider/models.rs:420-427`). `providers.md` states the code's order.
- `docs/SPEC.md:545` says a literal `key = "…"` is "refused with a warning"; the code gives the ordinary `unknown field` usage error, no tailored warning (`src/config/mod.rs:75-88`, `tests/it/providers_secrets.rs:300-310`).
- `docs/SPEC.md:560` says `doctor` exits 0 "when every provider checked has a usable key"; the code exits 1 only for the session's provider or an expired credential (`src/ipc/doctor.rs:42-48`).
- `docs/SPEC.md:560` calls `doctor` offline; it runs the same resolution as a session, so a configured `key_cmd`, `opencode auth export` or `refresh_cmd` runs (`src/ipc/doctor.rs:93`).
- `docs/reference/providers.md` (Retries section) omits the internal 60-attempt safety cap on every budget (`src/provider/retry.rs:20-21,111-118`).

## Source map

- src/provider/mod.rs
- src/provider/keys.rs
- src/provider/retry.rs
- src/provider/governor.rs
- src/provider/models.rs
- src/provider/usage.rs
- src/provider/stream.rs
- src/config/load.rs
- src/config/raw.rs
- src/config/resolve.rs
- src/config/types.rs
- src/config/mod.rs
- src/ipc/doctor.rs
- src/core/session/turnloop.rs
- src/core/session/toolloop.rs
- src/core/session/boot.rs
- src/core/session/mainloop.rs
- src/core/session/model.rs
- src/features/models.rs
- tests/it/providers_auth.rs
- tests/it/providers_secrets.rs
- tests/it/providers_retry.rs
- tests/it/providers_governor.rs
- tests/it/providers_status.rs
- docs/reference/providers.md
- docs/reference/config.md
- docs/reference/env.md
- docs/SPEC.md
