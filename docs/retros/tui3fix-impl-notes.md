# tui3fix implementer notes

Job `tui3fix`: the fixes from `docs/reviews/tui3-review.md` + addendum, brief
`docs/tasks/TUI3-fixes.md`. Worktree `kelir-wt/tui3`, branch `job/tui3`.

## What the review asked for, and what landed

| id | verdict | where |
|---|---|---|
| F-cancel | fixed | `src/tools_view.rs`: the fold's checked count is `checked()` (a `bash` call that did not fail and was not stopped); a stopped call keeps its own line `⏹ cancelled`, and the fold line names it (`✓ 2 calls · 1 stopped`). Review test un-ignored and extended. |
| F-edit | fixed | `CallLine::folds()` is `name == "bash"`; `edit` (any non-bash tool) keeps its own line inside the fold and is not counted. Review test inverted to the fixed expectation. |
| F-window | fixed | `src/session.rs::window_banner_line` puts one dim line under `cwd` when the window is not the provider's (default/cache with a reason, and a bare default); the normal `api`/`config` window stays off the banner. A start whose fetch has not landed cannot know yet, so `apply_list` writes the same line once when the answer is a fallback. `window_model` is seeded from `Prepared` in `Session::open`, so a `/model` swap announces the moved window once; `swap_model` announces the swap's own number so a swap before the first landing is not silent. |
| F-dev | fixed | `docs/DEVIATIONS.md` ("tui3 and its fixes") records the deviations from STEER-10 reqs 1-2 (no `done` summary, no per-call duration, no ambient cost/cache/steps) and where those numbers now live; `docs/design/TUI-v2.md` amended in place (amendment banner, facts row, summary line, tool verdict fields, width tiers, §8.1/8.2). |
| N-reach | tracked, STEER-8 | documented in DEVIATIONS and the design note: `usage` events in `events.jsonl`, the `state` object, tokens in the facts row; `kelir ctl status` is STEER-8 and not landed. Not fixed here. |

## Nits

Fixed:

- **N-count**: the fold line now names what it does not stand in for (`✓ 2 calls · 1 failed · 1 stopped`), so
  the arithmetic reads straight.
- **N-paused**: one blank row between the turn's final prose and the outcome line (`turn_summary` calls
  `separate()`), matching every other pair of blocks.

Left (each is pre-existing and outside this brief; none hides an action):

- **N-light**: the user echo and idle editor stay ANSI cyan (index 6). A terminal that remaps cyan very light
  gets low contrast on a light background. The colour is the maintainer's direction and was accepted by the
  review, so it stays; `docs/reference/tui.md` ("Spacing") now says the fix is the palette, not the
  transcript.
- **N-errrow**: a long provider error truncates the state row's next action at 80 columns. The row has no
  priority order for what to drop.
- **N-rlrow**: during a rate-limit wait at 80 columns the facts row falls to `working · 0 tok`; the state row
  still carries the countdown, so nothing is lost.
- **N-fold**: a blank row between two single calls from different steps (pre-existing group spacing).

## Notes for the reviewer

- The two tests that used to pin the bugs moved rather than weakened:
  `an_edit_inside_a_fold_is_not_named_anywhere_in_the_transcript` is now
  `an_edit_inside_a_fold_keeps_its_own_line`, and
  `a_cancelled_call_is_not_counted_as_a_checked_call` lost its `#[ignore]` and gained the positive assertions
  (`2 calls`, `1 stopped`, the stopped line). Both assert the fixed behaviour, not a weakened one.
- New e2e (pty, real binary, scripted mock): `the_tui_banner_names_a_window_only_when_it_fell_back` (a `/models`
  that answers 503), `the_tui_banner_is_silent_about_the_providers_own_window`,
  `a_model_swap_announces_the_new_window`, `an_offline_start_says_the_window_it_fell_back_to` (the brief's
  dead-port case, no mock at all) and `a_slow_list_is_applied_to_an_idle_tui_session`. New in-crate unit
  tests: `the_banner_names_a_window_only_when_it_is_not_the_providers` and
  `a_window_that_changes_after_a_swap_is_announced_once` (needed `Session`'s privacy, as the review said).
- **A real bug the offline test found.** `Session::idle` polled the metadata fetch once for
  `IDLE_MODELS_WAIT` (100 ms) and then blocked on the verb channel, so an answer that took longer than that
  — a dead port here takes ~2 s to refuse — was never applied until a turn started. `state` and the banner's
  note stayed on `fetching` for as long as nobody typed, which is exactly the window F-window is about. The
  idle loop now `select!`s the verb channel against the next look at the fetch while one is running. Both
  new pty tests were checked against the un-fixed loop (they fail there, pass here), so they pin the fix
  rather than the symptom.
- One judgement call: the banner suppresses the `"the model list: fetching"` note. It is not a degradation,
  it is "not known yet", and the real reason follows from `apply_list` a moment later; showing both would put
  two window lines on the same screen for a start that turns out fine.
- The dim banner line is a `BannerLine { text, dim }` rather than a second `push_text` path, so the ordering of
  the banner is unchanged and only the one note is dim.

## Follow-ups

- STEER-8's `kelir ctl status`, and the TUI showing it on request, is what closes N-reach.
