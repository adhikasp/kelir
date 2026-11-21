# steer9 implementer retro

## What was done
- Audited 101 fixed sleeps/timeouts/polls across `tests/` and `tests/support/mod.rs` and the
  gate logs' retry inventory. Replaced ordering-assumption sleeps with event-driven waits:
  file/process/socket/event conditions, gated mock replies and held bash calls, all under one
  90 s hang ceiling with a diagnostic dump on timeout.
- Fixed a real product bug the load run exposed: date-change sources compared against the
  event's wall-clock timestamp rather than the `KELIR_TODAY` date frozen in the system prompt.
  Red unit test first, then the minimal fix (`jobs::clock_source_from` plus
  `Session::rendered_date`). The e2e test that was failing 3/3 under load now passes alone and
  under stress.
- Added the gate load stress mode (`--load`, 3 rounds, 8 burners, no retries), a descendant-PID
  watchdog that cannot be tripped by sibling workers, and `scripts/flake-rate.sh` for rates.
- `docs/reviews/FLAKES.md` records the BEFORE/AFTER rates and the full inventory; measured on
  this box with the same script for both sides.

## What I would keep
- One `CEILING` constant with a dump-on-timeout is much more diagnosable than dozens of local
  magic numbers. Every slow failure now names its pid, events and stderr.
- Gated replies (`ModelsAnswer::gated`, `HOLD`/`release`) turn "slower than the turn" into an
  ordering the test controls, which is the only reliable way to test concurrency on a shared box.

## What I would change
- I started with a large WIP already in the tree and had to reconstruct its intent from the diff
  and the logs. A short in-tree notes file at the start would have saved a pass.
- The 30 MB credential-export test still rides a 5 s product timeout; under 8 burners it can
  cross it. The stress mode is harsher than production. I kept the test and the product bound
  intact and recorded the measurement instead of weakening either.
