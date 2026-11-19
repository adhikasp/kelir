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
