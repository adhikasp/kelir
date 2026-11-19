# STEER-8 — spend and status you can see (retro M5-A F9)

All 320 Go usage events said `cost 0.0`, so spend was invisible, and the driver learned about 200K-token prompts only
from the 429. Make resource use visible.

## Build
- **Cost**: when the provider reports no cost, compute it from a small built-in price table (per model, per 1M
  input / cached input / output tokens; the table is data in `src/models.rs`-adjacent file, overridable in config,
  stamped with a date; unknown model = no estimate, never a wrong one) and record `cost` plus `cost_source`
  (`provider` | `table` | `unknown`) in `usage`. Provider-reported cost always wins.
- **`kelir ctl <session> status`** (verb `status`, also shown in the TUI and `kelir ls` later): steps used/limit
  (limit only if set), turn count, last prompt size and context window percent, cache hit ratio (last / session),
  total tokens and estimated cost, last error and retry state (STEER-5), running tool calls, bg jobs, children.
  JSON with `--json`.
- **Warnings**: a `source`-style note to the log (and the TUI status line) when the last prompt exceeds a
  configurable `warn_prompt_tokens` (default 150000) or cost passes `warn_cost` (default unset); it informs the
  human/driver, never the model unless configured.
- Tests: price-table arithmetic (rounding, cached tokens), provider-cost precedence, status verb output on the mock
  (before/after steps, with a retry in progress), warnings fire once per threshold crossing. Windows + Linux gates.
