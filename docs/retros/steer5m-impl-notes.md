# STEER-5 merge — implementation notes

Brief: `docs/tasks/STEER-5-merge.md`. Branch `job/steer5`, worktree
`kelir-wt/steer5`. This is the merge of `main` (`de8e94a`, after STEER-6 and the
M5-A fixes) into the STEER-5 branch, reviewed round 2 as
`docs/reviews/steer5b-review.md`.

## The merge

`git merge main` conflicted in exactly the four files the brief named:
`src/session.rs`, `src/lib.rs`, `docs/DEVIATIONS.md`, `docs/PROGRESS.md`.

* `src/lib.rs`: both sides added a module (`signals` from STEER-5, `shellcmd`
  from STEER-6). Both are kept.
* `docs/DEVIATIONS.md` and `docs/PROGRESS.md`: both sets of entries kept, in
  merge order (STEER-5's first because its heading was already first; STEER-6's
  after it), nothing renumbered.
* `src/session.rs` — the real work. Both sides rewrote `stream`:

  - STEER-5 replaced the fixed `provider::backoff` loop with
    `retry::Budget` + `retry::delay`, added the governor slot wait, the
    interruptible `wait_for`/`take_wait_verb`, the `Waited` outcome and the
    `paused` outcome, and made `stream` take a `RequestFor` so a retry can be
    rebuilt (new model / new fold).
  - STEER-6 added the 400-named-field retry (`drop_refused_field`,
    `refused_fields`), clone-per-attempt `request` handling and the summary's
    one-request rule (which STEER-5 also had, in `budget_wait = ZERO`).

  The resolution keeps STEER-5's loop shape (budget, governor, `attempt()` as a
  separate future-taking method, `wait_for`) and folds STEER-6's two behaviours
  into it:

  1. `drop_refused_field` is called on the failed attempt *before* the retry
     decision. It mutates the request `attempt()` just used, records a
     non-retryable `error` note, and re-sends at once (`continue`) without
     charging the waiting budget — a 400 is an answer, not an outage.
  2. Because STEER-5's loop rebuilds via `request.build(self)` and STEER-6's
     gate lives in `Session::step_request`, `RequestFor::Step::build` now calls
     `session.step_request(&messages)` instead of `client.build(...)`. That
     keeps `parallel_calls`, the `reasoning_effort` gate, and the
     `refused_fields` set — otherwise a retried step would have re-sent a field
     the provider had just refused, and the STEER-6 e2e would have caught it
     (`tests/e2e_steer6.rs::a_400_naming_the_effort_is_retried_once_without_it`
     asserts exactly the later-step case).
  3. The compaction call site keeps STEER-5's
     `stream(rx, RequestFor::Built(&summarising), true)` — a summary is built
     once, from the model it was made with — with STEER-6's
     `self.capabilities(...)` wrapper around `compact::summarise_request` kept.

  The merged `error` event carries STEER-5's fields (`status`, `attempt`,
  `retry_in_ms`) *and* is used for STEER-6's field-drop note (with
  `retry_in_ms: None`, because nothing is waited for).

* The unit test fixtures in `session.rs`'s `mod tests` needed the new fields
  from both sides (`refused_fields`, `compact_at_tokens`,
  `reasoning_effort`/`reasoning_effort_explicit`, and
  `provider::build_request`'s new `parallel_calls` argument).

No test was dropped: lib went 328 (STEER-5 branch) / 334 (steer6 fixes round 2)
to 370, `e2e_steer6` 13 → 14 (the merge's copy has both probes), and every
pre-existing suite is intact.

## Review nits

* **N1** (the `model` ack during a wait said `applies: "next_step"` and
  `state.model` still reported the old model, although the swap had already
  happened). Fixed properly rather than by rewording. `accept` was split into
  `parse_verb` + `log_verb` + two answer paths:

  - `accept_at_once` logs the verb, applies it *then* answers
    `applies: "now"` with `self.state()` — so `state.model` is the new model;
  - `take_wait_verb` routes `say --now` and `model` through it (a small
    `at_once_verb(&request)` reads the raw request before parsing, since the
    choice of answer path has to be made before `accept` answers).

  `tests/e2e_steer5.rs::model_during_a_wait_retries_at_once_on_the_new_model`
  now asserts `applies == "now"` and `state.model == "go/model"`. `later` is
  untouched (`applies: "turn_end"`), because STEER-5 review N3 asked only for
  the doc line.
* **N2** (`Retry-After: 0` floor) is left as is: the brief says document only,
  and DEVIATIONS already states the floor and why (STEER-5 review S2).
* **N3**: `docs/DEVIATIONS.md` now has a paragraph naming the brief's line
  ("`later` ... takes effect immediately") against the code's boundary:
  `later` is *accepted and queued* at once (a delivery) but keeps
  `applies: "turn_end"` and does not cut a wait.

## F1 — the flaky test

`tests/e2e_verbs.rs::stop_during_a_provider_backoff_is_obeyed_at_once` used
the suite default schedule (`retry_base_ms = 200, retry_cap_ms = 400,
retry_max_attempts = 3`) against three 500s. On a loaded Windows box `ctl stop`
could be scheduled after all three attempts had spent the budget, so the turn
truthfully ended `paused` and the test failed — a test race, not a product bug.

Fix: the test now passes its own config,
`retry_base_ms = retry_cap_ms = 5000, retry_max_attempts = 3,
retry_max_wait = 60`, so the wait the first `error` event announces is five
seconds long, and it asserts on the announced event
(`error["retry_in_ms"].is_some()` — the session is waiting, not retrying)
before sending `stop`, then asserts `outcome == "stopped"` within a generous
15 s ceiling and that **no** retry went out (`mock.request_count() == 1`). No
other test was touched.

## Gates

| Gate | Windows (this box) | Linux (`scripts/remote-check.sh the maintainer kelir-verify-steer5m`) |
|---|---|---|
| `cargo fmt --check` | clean | see PROGRESS row |
| `cargo clippy --all-targets -- -D warnings` | clean | |
| `cargo test --no-fail-fast` | 370 lib + 22 e2e suites, 0 failed, 2 ignored (Unix-only signals) | |
| `e2e_steer5` × 10 | 10/10 green (17 tests each) | |

## What a reviewer should look at hardest

* `Session::stream` (the merged loop): the order is governor slot → retry
  `before_step` boundary → build → budget `record_attempt` → `attempt()` → drop
  slot → `drop_refused_field` (re-send at once) → retry decision → `error`
  event → `wait_for` → governor re-taken on the next loop.
* `RequestFor::Step::build` delegating to `step_request` — that is the one line
  that keeps STEER-6's capability gate alive across a retry.
* `accept_at_once` / `take_wait_verb`: the ack ordering (log, apply, answer) and
  that a `stop` still ends the turn *before* the answer goes out.
* `tests/e2e_steer6.rs` and `tests/review_steer6b.rs` are the STEER-6 probes the
  merge must not have weakened; they are unchanged and green.
