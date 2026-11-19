# STEER-5 — retry with backoff on provider errors (429 first)

Observed 2026-11-19: five concurrent kelir sessions on the OpenCode Go provider each failed with
`http 429 rate_limit_error` after ~15 steps; kelir retried briefly and then ended the turn in `error`.
A fully autonomous agent must ride out transient provider errors.

## Build
- **Retryable classes**: 429, 408, 5xx (incl. 529/overloaded), connect/DNS/TLS errors, timeouts, and a
  mid-stream disconnect **before any tool call of that step has started** (the step has no side effects yet; the
  request is simply re-sent from the same fold). Never retry 400/401/403/404 (401/403 once after re-reading the
  credential, STEER-2). Note `retryable` already exists on the `error` event.
- **Backoff**: honour `Retry-After` (delta-seconds and HTTP-date) and common reset headers
  (`x-ratelimit-reset*`, `anthropic-ratelimit-*`-style, OpenRouter's) when present; otherwise exponential with **full
  jitter**: base 2 s, factor 2, cap 120 s. Total budget per request `retry_max_wait` (config, default 30 min; `0` =
  unlimited); attempts are unlimited inside the budget. The clock and the RNG are injectable (tests).
- **Visibility**: every wait writes an `error` event (`where:"provider"`, `retryable:true`, `status`, `attempt`,
  `retry_in_ms`, short reason, never the body of secrets) and a visible line in the TUI / human output
  ("rate limited (429): retrying in 34 s, attempt 5, say stop to cancel"); NDJSON carries the event; Herdr status
  stays `working`; the status line shows the countdown.
- **Control**: the wait is interruptible: `stop`, `say --now`, `later`, `model` (the retry uses the new model),
  `end` and a signal take effect immediately; a stopped wait ends the turn `stopped`, not `error`.
- **Exhaustion**: after `retry_max_wait` the turn **pauses** (`turn_ended` outcome `paused`, message "provider
  unavailable after N attempts / M minutes; say continue to retry"); a later `say` starts a new turn and retries
  from the same fold. (If STEER-4's `paused` outcome is not on your branch yet, add the outcome minimally in the
  same shape as the STEER-4 brief so the later merge is trivial.)
- **Client-side concurrency governor (opt-in)**: `max_concurrent_requests = N` per provider in config, enforced
  across local kelir processes with advisory file locks under `<state root>/locks/<provider>-<slot>` (a crashed
  holder must not leak a slot: locks are released by the OS). Default off. When waiting for a slot, the status
  line says so. Purpose: several local workers share one account limit.
- Docs: SPEC.md (Providers or Loop), DEVIATIONS.md, README/usage; the `kelir doctor` output (STEER-2) may show
  the effective retry settings.

## Tests
Unit: backoff schedule with an injectable clock/RNG (bounds, cap, full-jitter distribution), `Retry-After`
parsing (seconds, HTTP-date, garbage, huge), reset-header variants, classification table, budget accounting.
E2E (mock server): 429 with `Retry-After: 1` twice then success (the step succeeds, events show two waits, no
turn error); 429 without header follows the schedule (scaled-down config); 5xx then success; mid-stream
disconnect before tools -> retried, after a tool call started -> not retried (turn error, tool unknown outcome
per M3); `stop` during a wait ends the turn `stopped` within 1 s; `model` swap during a wait; budget exhaustion ->
`paused` and a later `say` retries; two kelir processes with `max_concurrent_requests = 1` never overlap
requests (mock records concurrency) and a killed holder frees the slot; secrets never in events. Windows and Linux.
Gates and rules as in AGENTS.md (no leaked processes, no image-name kills).
