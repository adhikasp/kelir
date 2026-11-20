# STEER-8 review (spend and status you can see)

Reviewed `f02e5b3..e30756b` on branch `job/steer8`. Reviewer: fresh QA agent.

## Verdict: PASS WITH FIXES (0 blockers, 3 should-fix, 4 nits)

## Gates
- Windows `scripts/gate.sh`: fmt ok, clippy -D warnings ok, nextest **1013 passed, 3 skipped**, doctest ok. PASS.
- Linux `scripts/remote-check.sh the maintainer kelir-verify-steer8-rev`: fmt ok, clippy ok, nextest **1015 passed, 3 skipped**, doctest ok. PASS.
- Added `tests/review_steer8.rs` (5 tests, all pass on Windows): a resume does not repeat a warning the log already owes;
  `warn_cost` counts table estimates and ignores unknown-cost models; mixed provider+table sessions are `mixed` with the
  right sum; `status` sees a running call and a bg job (and 0 after `cancel`); a wrong token is refused with no state leaked.
- Independent arithmetic: `scripts/live/steer8_check.py` recomputes tokens, cached tokens, cost sum and session cache ratio
  from raw `events.jsonl` and compares with `ctl status --json`: **match** (both live sessions, 4 usage events each).

## Findings
| # | Sev | Where | Finding |
|---|-----|-------|---------|
| 1 | should-fix | `src/spend.rs:67-76` (TABLE, `deepseek/deepseek-v4.1-flash-1017`) | The default live model's row is 0.30 / 0.006 / 1.20 $/M, the *peak* line. OpenRouter's listing for `-1017` is time-of-day priced (0.15/0.6 off-peak, 0.30/1.2 peak), and what it actually billed in the live runs is lower still (about 0.08-0.10 $/M input). Recomputing the table estimate against the 8 real usage events gave **1.3x to 4.5x too high** (aggregate 3.97x on run 1, 3.1x on run 2). The brief says "unknown model = no estimate, never a wrong one"; this row is a wrong one. Impact is bounded because OpenRouter reports cost and the provider always wins, so the table only shows if a gateway omits cost. Fix: drop the row (or mark time-varying models unknown), or label table figures as an upper bound; and stamp `PRICES_AS_OF` honestly per row. The other rows (`-flash`, `-pro`, `v4-flash`) match the public listing. |
| 2 | should-fix | `src/session.rs` `status_answer` / `Live.window` | Before the first request `status` reports `window: 128000` (a default), then `1048576` once the models API answers. A driver comparing percent across the two sees a jump; `percent` is null at that point so nothing wrong is printed, but `window` should be `null` (or carry `window_source`) until known. Seen in `st0.json` vs `st1.json`. |
| 3 | should-fix | brief vs impl | The brief says status is "also shown in the TUI". The TUI shows only the warning line and the existing facts row; `ctl status` is the only surface. The implementer chose this (impl notes "facts row stays lean (tui3)") but it is not in `docs/DEVIATIONS.md`. Add the deviation entry (or a `/status` TUI command). |
| 4 | nit | `src/spend.rs` `warnings()` | Message says "estimated cost" even when the number is the provider's own, and `$0.0003 over warn_cost ($0.0003)` prints identical rounded figures (value 0.000309 vs 0.0003). Print more digits for small amounts. |
| 5 | nit | `resolve()` | A provider-reported cost of exactly 0.0 on a priced model is replaced by a table estimate. Intended for Go, but a genuinely free/BYOK request on a priced model gets a made-up cost labelled `table`. Acceptable; document. |
| 6 | nit | `from_events` | A crash between a `usage` event and its `warning` event loses the warning forever (resume arms against the state). Edge case; no data loss. |
| 7 | nit | `children` | Hard-coded 0 (documented). |

## Acceptance lines
- Price table, config override, dating: `PRICES_AS_OF` printed in text and JSON; `[prices."model"]` overrides, validated (`deny_unknown_fields`, negative refused). OK, except finding 1.
- Provider cost wins; unknown model = `unknown`, `usd: null`, no estimate: OK (e2e + my mixed test). `cost_source` recorded on every usage event incl. compact.
- `ctl status` text and `--json`: all fields present on every state (null when unknown); verified at zero steps, during a running tool call (`running_calls` 1), after the turn, on a resumed session (`turn_steps 0`, totals rebuilt from the log), in a retry wait and when paused (e2e). bg jobs counted and cleared on cancel. Read-only, not logged.
- STEER-7: `status` needs the session token like every verb (one rule, documented in the verbs row); `kelir ctl` reads the token file; a wrong token gets a refusal and no state.
- Warnings: once per crossing (live: four consecutive over-threshold prompts, one warning; falls back and re-arms per e2e), not repeated after resume (my test), never in a request body (e2e scans request bodies), one dim `Info` line in the TUI placed between turns/folds without disturbing them (live pane), one stderr line in plain mode.
- Docs: `docs/reference` tables updated; drift test passes in the gate.

## Live drive (OpenRouter, `deepseek/deepseek-v4.1-flash-1017`, Herdr panes, `scripts/live/steer8_live.sh`)
1. Keep-alive run, `status --json` at start, during `sleep 25` (1 running call, 2 requests, $0.00023521 provider), after the turn (4 requests, 5317 prompt, 2048 cached, 215 out, $0.000314911, session cache 38.5%), `end`, then `run --resume` + status: totals identical, `turn_steps 0`. Matches the log exactly.
2. TUI pane with `warn_prompt_tokens=1100`, `warn_cost=0.0003`: one prompt warning line after the first step, one cost warning after step 4; both in `events.jsonl`; folds and the `ok` outcome lines unaffected.
3. Table-vs-provider comparison: see finding 1 (ratios 1.29-4.49).
Not live-verified: Go provider (no key); retry-wait status is mock-only. No leftover `kelir.exe`; panes removed.

## Not covered
Retry-wait and paused status live (mock only); Go path.
