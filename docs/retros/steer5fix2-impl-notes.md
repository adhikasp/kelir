# STEER-5 fixes, round 2 — implementation notes

Brief: `docs/tasks/STEER-5-fixes2.md` (a lost SIGINT during a retry wait on
Linux). Branch `job/steer5`. Commits: `43a92c7` (the merge), `a0027a7` (the
merge's new fields), `a3f803b` (the fix), `3516666` (the tests), `2a6ce1d` (the
install order), and the docs commits after them.

## The reproduction

The Linux gate's own loop, on `the maintainer`, `kelir-verify-steer5b`:

```
cd ~/kelir-verify-steer5b/src-tree
for i in 1 2 3 4 5 6 7 8 9 10; do cargo test -q --test e2e_steer5; done
```

Before the fix: **4 of 10 runs failed** (the brief saw 2 in 6), always
`an_interrupt_during_a_wait_ends_the_turn_stopped`, always with the same
signature — `no event within 30 s (session pid … alive=true, seen so far:
[session_started, user, step_started, error])`. The session is healthy, it is
still *waiting*, and the SIGINT is simply gone.

Inserting a 500 ms sleep between `wait_for("error")` and `interrupt()` made it
10/10 green. That is the diagnosis in one line: **the test was not racing the
wait, the signal was arriving before the wait existed and nothing remembered
it.** The brief's own suspect list had it right ("the test sends SIGINT before
the listener exists"). It is not load: the same binary, same runner, is green
alone every time.

## The root cause

Two independent races, both of the same shape:

1. **The level was edge-triggered.** `signals::interrupted` subscribed to a
   `watch<u64>`, recorded the value it saw at subscribe time as its baseline,
   and resolved when the value moved. The handler bumped the counter with
   `watch::Sender::send`, and **`send` discards its value when there are no
   receivers at that instant** (tokio's own source, `sync/watch.rs`: `if 0 ==
   self.receiver_count() { return Err(SendError(value)) }`). A wait that
   subscribed *after* the interrupt therefore took the already-bumped value as
   its baseline and waited for ever. This is the whole bug: the interrupt is
   state, not an event, and the code treated it as an event.

2. **The listener was installed too late.** `signals::install()` sat in `drive`
   just before `session.run(..)`, but `prepare(..)` — which records
   `session_started` — runs *before* that. A client that reads `session_started`
   off stdout can signal a process that has no handler yet; the default
   disposition then kills it (or, under a handler-free window, drops it).
   My own new e2e caught this one: it failed with "the session closed its
   output", i.e. the SIGINT arrived before `install`.

Neither is a scheduling problem, so no amount of retrying the test would have
fixed it — and the old suite was green enough to hide it.

## The fix

`src/signals.rs` is now a **level** that is checked on entry *and* while
waiting:

- `Listener { requested: AtomicBool, count: AtomicU64, wake: Notify }`. The
  handler's body is `post()`: bump the count, **set the level, then**
  `notify_waiters()`. The order matters — a wait that checks the level and then
  parks must see either `true` (arrived earlier) or the notification (arrived
  after it parked), never neither.
- `interrupted()` loops: create `Notified`, **`enable()` it** (a `Notified`
  joins the waiter list when enabled or polled, not when created, and
  `notify_waiters` stores no permit), *then* check the level. A pending
  interrupt returns immediately; otherwise it parks.
- `take()` clears the level and reports whether there was one. A wait calls it
  when it acts on `interrupted()`, so **one Ctrl-C ends one turn**; a wait that
  ends for another reason (`stop`, `say --now`, the deadline) leaves the level
  set, which is what "pending for the next wait" means.
- `install()` is now `Once::call_once` (two sessions starting together must not
  leak two listeners) and **creates the platform streams before returning**, on
  the caller's task — creating a `Signal` is what installs the OS handler;
  polling it is not. `Streams::new()` therefore runs inside `install`, and the
  spawned task only waits on what already exists.
- `session::drive` calls `install()` on its **first line**, before `prepare`
  records `session_started`. There is no window in which an observable session
  has no handler.

Everything else is unchanged: the double-interrupt escape hatch
(`exit 130` within `DOUBLE`), the "a signal is a `stop`" outcome, the
`--keep` session surviving it, and the Unix/Windows split of what is watched
(`SIGINT`+`SIGTERM`; `CtrlC`+`CtrlBreak`).

## The tests

Three layers, and each one was checked against the **old** mechanism, because a
regression test that passes on the bug is not a regression test:

1. `src/signals.rs` unit tests (the state machine, no signal sent — see below):
   - `an_interrupt_with_no_waiter_parked_is_not_lost` — post, then wait; the
     level alone must answer. **Fails on the old code.**
   - `a_parked_wait_is_woken` — wait, then post. **Fails on the old code**
     (the old `interrupted()` was not `Send` enough to be driven this way, and
     the baseline it took was already stale).
   - `two_hundred_signals_before_their_wait_are_all_seen` — the brief's
     "200 iterations of start → wait → SIGINT → expect stopped within 2 s",
     done in process (a process per round would be 200 spawned sessions for
     the same assertion the e2e already makes once). **Fails on the old code.**
   - `the_listener_installs_once_and_waits_are_pending_until_a_signal` and
     `a_wait_dropped_mid_poll_leaves_the_listener_alone` — the plumbing,
     unchanged in intent.
2. `tests/e2e_steer5.rs`:
   - `an_interrupt_during_a_wait_ends_the_turn_stopped` (the gate's own test,
     untouched) — signals a wait that is already running.
   - `an_interrupt_before_the_wait_begins_is_not_lost` (new) — drives the lost
     wakeup *deliberately* instead of hoping for it: the mock holds the 429 at
     a gate (`Reply::GatedHttpWith`, new in `tests/support/mod.rs`), so the
     session cannot have entered a wait yet; the test signals, lets the handler
     run, then opens the gate. 20 rounds, each one a fresh session, each round
     asserting `outcome == "stopped"` within 2 s.
     **Fails on the old code** with exactly the gate's signature: `round 0: the
     interrupt was lost; the turn never ended (seen: [session_started, user,
     step_started, error])`.
3. A 500 ms-sleep variant of the gate's test was used as the diagnostic and is
   not committed (it is a stronger test of the same property that the gated test
   already covers without the sleep).

## Verification

- Windows: `cargo fmt --check`, `cargo clippy --all-targets -D warnings`,
  `cargo test` — all green (numbers in `docs/PROGRESS.md`).
- Linux (`the maintainer`, `kelir-verify-steer5b`): `scripts/remote-check.sh` green, and
  the brief's loop — `cargo test --test e2e_steer5` ten times — **10/10 green,
  19 tests each** (it was 4/10 red before), plus the whole suite ten times.

## What the merge cost

`git merge main` conflicted in `docs/PROGRESS.md` (both branches added a row at
the same place: main's `M5-A fixes (landed)` and this branch's STEER-5 fixes
row; kept both, main's first because it landed first) and — unseen by the
conflict — main had added fields to two types this branch constructs:
`EventBody::Error` gained `status`, `attempt` and `retry_in_ms`, and `Config`
gained `prompt_path`. `a0027a7` fills them in (all `None`: these are
`source`/`log`/`verb` errors with no HTTP status, and the test session's prompt
is the embedded default, which has no path). **The branch did not build after
the merge until that commit** — worth knowing, because it means the merge was
never "clean but wrong": `cargo build` is the first gate for a reason.
