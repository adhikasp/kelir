# STEER-5 fixes — implementation notes

Brief: `docs/tasks/STEER-5-fixes.md` (review FAIL: 2 blockers, 4 should-fix,
4 nits). Branch `job/steer5`. Commits: `f24e671` (the merge), `913bd45` (B2,
S1-S4, nits), `166f657` (the signal listener), `3b95ba3` (Date/floor tests and
one de-flaked e2e). Every finding is addressed; the reviewer's own suite passes
unweakened (it is untouched: `git diff 9bb5ca4 -- tests/review_steer5.rs` is
empty).

## B1 — the merge

`git merge main` conflicted in `docs/SPEC.md`, `docs/DEVIATIONS.md` and
`src/resume.rs`, and auto-merged `src/session.rs` into two `TurnEnd::Paused`
variants, as the review predicted.

- **One variant.** `TurnEnd::Paused(PauseReport)`; `PauseReport` carries
  `reason` (`PauseReason::Steps | Provider`), the attempt count, the time waited
  and the provider's short reason. `PauseReport::steps(n)` is the STEER-4
  constructor, and `message()` returns either "turn paused after N steps; say
  continue to resume" or "provider unavailable after N attempts, M: …".
- **One outcome word, one reason field.** `turn_ended` gained an optional
  `reason` (`steps`/`provider`), so a reader — and the review's own "which pause
  was this" question — is answered without guessing from the message.
- **One Herdr rule.** `blocked` for both pauses: STEER-4's reasoning ("a pause
  *is* waiting for a human; this Herdr drops `paused`, measured with
  `scripts/live/m2_osc_probe.py`") applies unchanged to a provider pause, and
  the STEER-5 text that said `idle` is gone. `e2e_tui`'s expectation changed to
  match, and it now asserts the `reason` too.
- TUI: one `Phase::Paused` and one `TurnEnded` arm (the merge had doubled both).
- Docs: the `turn_ended` row and both DEVIATIONS blocks are unified; the rest of
  main's docs merged clean.

## B2 — the wait is interruptible by the verbs that say "now"

This was the real work. `wait_for` used to take verbs and push everything but
`stop`/`end` to the scheduler, so `say --now` and `model` were applied *after*
the wait — at the publisher of a `Retry-After` that may be seven days long.

- `wait_for` now returns a three-way `Waited` (`Timeout`, `Retry`, `End`).
  `say --now` and `model` are **applied immediately** and the wait returns
  `Retry`: the request is rebuilt (new fold, new model) and re-sent at once,
  with a **fresh budget** and **the new provider's governor** (the review's
  "a swap to a provider that is not rate limited still sits out the old
  provider's wait").
- `apply` was split into `apply`/`apply_pending` so the boundary path and the
  wait path share one implementation — no behaviour is duplicated.
- `stop`, `end` and a signal end the turn; `later`, `tools`, `compact` and
  `skills` keep their boundary (a `say` that is merely queued still joins at
  `before_step`).
- The answer stays honest: `applies: "now"` is now true. The e2e asserts the
  ack *and* the second request arriving within the test's window.

**The signal.** `src/signals.rs` (new). One listener per process, installed
before the first turn, and every wait subscribes to a `watch` channel. Two
things I got wrong first and fixed:

1. *Lazy registration per wait* looked tidy (Tokio's `Signal` "for the duration
   of the entire process" caveat aside, the intent was to leave a Ctrl-C during
   a request fatal). It is a race: the signal can arrive between the logged wait
   and the wait's first poll, and then the default disposition kills the
   process. That is exactly what the Linux gate caught. Registering once at
   start removes the race.
2. *Tokio never restores the default disposition* once a `Signal` exists (its
   own docs), so a handler installed for a wait would capture Ctrl-C for the
   rest of the session and silently ignore it. Hence the second rule: **a second
   interrupt within two seconds exits** (`exit 130`), which is the way out of a
   wedged session and the same rule the TUI states.

I also killed my own shell with a test: the first version of the e2e sent
`CTRL_BREAK_EVENT` via `GenerateConsoleCtrlEvent`. A Windows console control
event addresses a whole *console*, not a pid, so it took down the test runner
and the console it ran on. The test is now Unix-only (`SIGINT` to the child's
own pid) and the Windows half is a unit test of the registration itself; that
split is written down in DEVIATIONS as the remaining honest gap.

## S1 — the governor follows the model swap

`swap_model` rebuilds the governor from the config entry the swap resolved to
(`governor_for`), so a swap to a governed provider starts taking slots and a
swap away stops being held to the old limit. Compaction summary requests take a
slot too (the `!summary` guard is gone): they are provider requests on the same
account, which is the whole point of the knob.

## S2 — a hint can never make a hot loop

`retry::delay` lifts a provider hint to `min(250 ms, retry_base_ms)`: a gateway
answering `429 Retry-After: 0` for ever now gets at most four requests a second
and, with the default schedule, not even that. A fast configured schedule keeps
its own shorter floor, so tests and tuners are not overruled. `MAX_ATTEMPTS`
also dropped 200 → 60: with the default schedule the waits reach the 120 s cap
after seven attempts, so the count is a safety net, not the limit.

## S3 — the message never carries the body

A retryable provider error's `message` is now `http <code>: <class>`
(`http 429: rate limited`). The body is still read (to drain the connection) and
then dropped. A *refusal* (400/401/403/404/422) keeps its body — "unknown model"
is the answer a human needs — bounded at 500 chars and with the session's own
key replaced by `[redacted]` (`util::redact`). The old DEVIATIONS sentence
("a credential can only appear in the session's own log") was the actual bug:
NDJSON stdout is not the log.

## S4 — docs match the code

SPEC: the `turn_ended` reason, the interruptibility paragraph (which verbs end a
wait, which keep their boundary), the hint floor, the `error` row's fields and
its "never a body" rule, the governor following a swap. DEVIATIONS: the unified
pause, the wait's `now` verbs, the signal rule and its test gap, the redaction,
"NDJSON carries the wait as the `error` event, not a separate line", the
governor/swap rule, the `Date`/tightest-window rule, and the summary-is-tried-
once choice.

## Nits

- N1: `header_hint` measures absolute hints from the response's own `Date`.
- N2: the **shortest still-closed** reset window wins (a zero/expired window is
  not a window at all), and a `Retry-After: 0` beside a real window yields the
  window.
- N3: a failed summary is skipped, not retried — documented, with the reason.
- N4: `RetryingDone` is emitted on the `Waited::End` path too, so no
  swap/`end` race can leave a stale countdown.

## What was hard / where I was misled

1. **The merge was the easy part; the wait was not.** "Interruptible" reads like
   a `select!`, but the verbs differ in what interrupting *means*: two of them
   ( `say --now`, `model`) must end the *wait* and re-send, three (`stop`,
   `end`, signal) end the *turn*, and the rest must not cut it at all. The
   three-way return is what made that legible.
2. **The signal has two nasty platform truths** (Tokio never restores the
   disposition; a Windows console event addresses a console, not a pid) and both
   are the kind that only shows up when a test or a person presses the key. The
   first cost a Linux-gate round trip; the second cost my own shell and is now a
   written-down gap.
3. **A test that asserts a wall clock is a flake with a timer.** The
   429-without-header e2e asserted "the whole run took under 1.6 s" and failed
   twice under load while passing alone — the exact shape STEER-9 describes. It
   asserts the `retry_in_ms` the log recorded now. The same reasoning is why the
   new interrupt tests assert on events, not on sleeps.
4. **Two other flakes** in the six full runs I did: `e2e_m4`'s
   `a_promoted_job_survives_a_killed_session_and_resume_folds_it_in` (once) and
   `e2e_verbs`'s `model_swap_can_change_the_provider` (once). Both pass alone and
   are named in STEER-9's inventory; neither is touched by this change, and
   neither is weakened here.
5. **A signature trap in the test harness:** the review's suite has its own
   `LiveSession::start` (`mock, mock, args, config` — two mocks, the second for
   the swapped-to provider), and my first e2e tests copied the one-mock form.
   The compile error was the cheap way to find out.
