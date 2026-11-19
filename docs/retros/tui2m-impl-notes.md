# tui2m — bringing `job/tui2` up to date with main (merge + resolve)

Brief: `docs/tasks/STEER-10-merge.md`. Branch `job/tui2`, merge of `main`
(STEER-5 retry/backoff/paused/governor, STEER-6 prompt lines, shell route,
multi-edit, `compact_at_tokens`, STEER-4, M5-A fixes) into it.

## What the five conflicts became

| file | resolution |
|---|---|
| `src/models.rs` | both field sets on `ModelInfo` (`vision` from tui2, `parallel_calls`/`reasoning_effort` from STEER-6) and both parse paths; the two sides' assertions on `OPENROUTER` were merged into one test body instead of one dropping the other |
| `src/prompt.md` | one prompt, both sides' lines, **943 bytes** (the `<= 1024` test still holds). The tui2 line (`kelir view` for an image pointer) and STEER-6's retro lines (plan once, batch reads, `git grep`, `target/`, no `python/sed patch`, never chain, `share no shell state`, name the proving command, AGENTS.md/CLAUDE.md, `{{skills_dirs}}`) are all there. Snapshot `tests/fixtures/prompt.md.snapshot` regenerated; the rule list in `sources::tests` is satisfied unchanged. |
| `src/session.rs` | main's `parse_verb` helper wins as the one place a verb is validated; tui2's `view` check (validated in the ack, not at a boundary) moved into it, so `accept` and `accept_at_once` both get it. `EventBody::Error` in the `view` path gained STEER-5's `status`/`attempt`/`retry_in_ms`. |
| `src/tui.rs` | one `status_text`: main's wait line (countdown to `until`, `seconds_text` so sub-second waits read `ms`) composed with tui2's one-word phase (`done` still reads `idle`). `UiEvent::Retry` (tui2's own shape) is **gone**; `UiEvent::Retrying` now carries `status: Option<u16>` and `attempt: u32` and feeds both rows — the facts line and the `rate limited` activity state. A slot wait (`status: None, attempt: 0`) shows in the facts row only. Both sides' tests kept; the duplicated facts-row test name is kept as one body under tui2's name plus main's name. |
| `docs/PROGRESS.md` | both rows, main's order kept. |

## The second half of the brief: the countdown and the paused rows

- A provider retry reaches the state row through the same event the facts row
  reads, so `rate limited · 429 · retrying in 34 s · attempt 5 · say stop` is
  driven by STEER-5's real events (not a UI-side guess), and `tick` re-reads the
  deadline from the wall clock instead of subtracting from a stale snapshot.
- The transient `note` no longer repeats the wait line: the facts row already
  carries it and the state row has its own countdown, and with both it read the
  same sentence twice in one frame.
- **A real bug the merge introduced, found by driving the binary**: a provider
  pause (retry budget exhausted) left the state row `⠋ thinking 0s · waiting for
  the provider` — the pause never left the wait. `Activity::turn_ended` now
  clears the countdown and enters `Paused`/`Error` by outcome, and the TUI feeds
  the pause message's reason (before `; say continue`) into the row, so it says
  `⏸ paused · provider unavailable after 2 attempts, 188 ms: provider error
  (503) · say continue`. Covered by `activity::tests::a_retry_that_ends_in_a_pause_reads_paused`,
  `..._in_an_error_reads_error` and the tui test that checks the reason replaces
  the role's default line.
- `PauseReport::message` and the TUI's pause notice now say `1 step`, not `1
  steps` (the review-N4 rule, applied where the merge reran it).

## Gates (Windows)

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, three full
`cargo test --no-fail-fast` runs: 739 / 736+3 / 742 passed. The 3-failure run's
failures are the recorded STEER-9 load flakes, each green alone and in its own
suite straight after (`e2e_verbs::say_now_stops_the_running_call_within_two_seconds`
— a two-second wall-clock bound; `review_m4b` b2/b7 — process-tree timing on a
box shared with sibling workers). Linux: `scripts/remote-check.sh`.

## Live drive (pty, scripted mock, `scripts/live/tui2_scn.py` + `tui2_server.py`)

Scenarios and what the replayed rows said (40x100, ConPTY, pyte):

| scenario | rows |
|---|---|
| idle | `● idle · ready` / `kelir idle · step 0 · 0 tok` |
| thinking | `⠹ thinking 1s · waiting for the provider · esc stops` |
| streaming | `⠹ writing 1s · ~10 tok · esc stops`, markdown `##` heading and inline code |
| tool | `⠋ running bash: sleep 2 && echo hello-from-tool 0s · esc stops`, then the one dim line `$ sleep 2 … ok · 16 B · 2.2s` |
| rate limited | `⠋ rate limited · 429 · retrying in 1 s · attempt 1 · say stop` and `kelir working · rate limited (429): retrying in 994 ms, attempt 1, say stop to cancel · 927 ms left` |
| paused (step cap) | `⏸ paused · step cap 1 reached · say continue`, `kelir paused · … · step 1/1`, notice `turn paused after 1 step; say continue to resume` |
| paused (provider) | `⏸ paused · provider unavailable after 2 attempts, 188 ms: provider error (503) · say continue` |
| error (401) | `✗ error · http 401: provider refused: {} · say to retry`, `kelir error · …` |
| done | `✓ 2 steps · 3.6k tok · $0.0024 · 5s`, then `● idle · ready` |

No real provider was called; every scenario used the in-repo mock.

## Where the brief and the tools misled

- The brief says five conflicts; `git merge main` produced exactly those five.
  What it does not say is that the *same* retry feature exists twice under two
  names (`UiEvent::Retry` on the branch, `UiEvent::Retrying` on main), and that
  the branch's version has no producer left after the merge — deleting the dead
  variant and widening the live one was the only way to keep one source.
- `cargo test` runs the suites in parallel and the STEER-9 flakes are wall-clock
  and process-tree bound, so a red run says nothing on its own; every failure
  here was re-run alone before being called a flake.
- A merge conflict in a *test* body can hide a semantic conflict: both sides
  asserted the same `OPENROUTER` fixture for different fields, and keeping one
  side's assertion would have silently dropped the other's coverage.

## Second merge: main at 997a833 (buildinfra, 566379c)

`git merge main` after the first one conflicted only in `docs/PROGRESS.md` (both sides appended a row).
Resolved by keeping both, main's newest last, and putting the buildinfra row (`BUILD-INFRA`) above mine;
no other file conflicted (`AGENTS.md`, `Cargo.toml`, `.cargo/config.toml`, `.config/nextest.toml`,
`scripts/gate*.sh`, `scripts/remote-check.sh`, `tests/buildinfra.rs` all auto-merged).

Nothing in tui2's code collides with the build infra: the new gate wants fmt, clippy, nextest -j 4 and
doctests, and the TUI suite already sits in a `pty` nextest group (max 2) via `.config/nextest.toml`,
so the PTY tests do not oversubscribe the box next to the process-heavy suites.

Gate at the merge commit 93930ac, `bash scripts/gate.sh --linux`:
fmt ok (1.2 s), clippy ok (50.5 s), nextest 757 passed / 2 skipped in 151 s with **zero retried tests**,
doctest ok, Linux via `scripts/remote-check.sh the maintainer`: rc=0, 757 passed, FMT-OK, CLIPPY-OK, gate: PASS.
The 2 skipped are the pre-existing Unix-only/Opt-In tests, not tui2's.
