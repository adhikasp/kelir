# STEER-8 implementation notes

Spend and status you can see (retro M5-A F9).

## What landed
- `src/spend.rs` (new): the price table (stamped `PRICES_AS_OF`), `estimate`/`resolve`
  (provider cost always wins; a reported 0.0 falls back to the table; no price = `unknown`,
  never a guess), `Tally` (folded from the log as it is written, so a resume rebuilds it),
  threshold warnings, `status_json` and `render_text`.
- `usage` events carry `cost_source` (`provider` | `table` | `unknown`); old logs without it
  are read by their cost.
- `warning` event (`prompt_tokens` | `cost`): written to the log, one dim `Info` line in the
  TUI, one stderr line in plain mode; the fold ignores it, so the model never sees it. Fires
  once per crossing; falling back under the line re-arms it; a resumed log does not repeat
  a warning it already owed.
- Config: `warn_prompt_tokens` (default 150000, 0 = off), `warn_cost` (unset), `[prices."<model>"]`.
- Verb `status`: read-only, answered where it arrives (mid request, in a retry wait, paused),
  never queued or logged. **It needs the session token like every verb** (one rule, no
  per-verb exception); `kelir ctl` reads the token file. Text by default, `--json` for the line.

## Decisions
- The TUI facts row stays lean (tui3); `ctl status` carries steps, cache %, tokens, cost.
- `children` is reported as 0 until sessions have children.
- Cached tokens are part of `prompt_tokens`, so the plain input rate applies to the rest.

## Tests
Unit: price arithmetic, rounding, cached > prompt, precedence, tally, warnings, status shape
(`src/spend.rs`). E2E on the mock: `tests/e2e_steer8.rs` (before/after a step, table/unknown/
provider cost, retry in progress, paused turn, prompt and cost warnings once per crossing).
