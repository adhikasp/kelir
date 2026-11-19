# Dogfood findings (running list; fed into the per-job retros)

- 2026-11-19 M5-A: kelir stopped with `kelir: step limit 100 reached` after ~100 tool steps, leaving the
  turn in `error`. `kelir run --keep` survived and continued on `ctl say` (limit is per turn). Findings:
  (a) the default 100 is too low for implementer jobs (user config now sets 400); (b) the message should
  say "turn paused, say 'continue'" rather than read as a failure; (c) `kelir ctl latest` picks the newest
  session dir even when it is dead/unrelated (use the id); (d) a status line with steps used/limit would
  have shown it coming.

- 2026-11-19 (maintainer decision, supersedes item (a) above): **no step limit by default**: `max_steps` unset or
  `0` = unlimited; the knob stays for jobs that want a cap; the 400 in the user config goes once the code change
  (STEER-4) lands. Item (b) still applies to an explicit cap: report "turn paused, say continue" (outcome
  `paused`), not a failure.

- 2026-11-19 concurrency: five kelir sessions at once on the OpenCode Go provider -> every new worker hit
  `http 429 rate_limit_error` after 14-17 steps; kelir retried (retryable) but gave up and the turn ended in `error`.
  M5-A's own requests carry ~200K prompt tokens per step (never compacts: window 1M, trigger at 80%), a big share
  of any token-rate limit. Findings: (a) 429 handling should honour `Retry-After` and back off for minutes, then
  *pause* the turn (not fail it); (b) compaction trigger at 80% of a 1M window is far too late for cost/rate: add a
  configurable soft target (e.g. compact_at_tokens = 150000); (c) workers re-read big docs (DEVIATIONS.md 44 KB,
  SPEC 26 KB) in full: skills/AGENTS.md should point to the relevant section, and `cat` of big files should be
  discouraged in the prompt (use sed -n ranges / rg); (d) concurrency limit of the provider unknown: measure and
  record (currently 3 concurrent workers is the working number).
