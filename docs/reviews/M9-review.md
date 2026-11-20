# M9 review — named sessions, jobs, single-flight, STATUS

Reviewer: fresh QA (kelir session, opencode-go/hy4-flash-free). Branch `job/m9rev`, diff
`3e4d1cb..9f0a223` (`git diff` read in full; 27 files, ~4590 insertions). Windows 11 dev box
(Git Bash) + Linux gate through a private work dir (`kelir-verify-m9rev`).

## Verdict: PASS WITH FIXES — 1 blocker, 3 should-fix, 2 nits

One blocker: a documented `jobs.toml` key (`max_steps`) is parsed and then dropped before the
session loop, so a job that declares a step cap runs uncapped. Everything else in the brief —
named sessions, provider/model/timeout/task_file overrides, single-flight with real processes,
STATUS/history, exit codes, trust and docs — works under adversarial tests. Three should-fix
issues are about edge behaviour and a live-drive gap; none of them can lose data or overlap two
turns.

## Pyramid (counts)

| layer | command | result |
|---|---|---|
| fmt | `cargo fmt --check` | pass |
| clippy | `cargo clippy --all-targets -- -D warnings` | pass |
| unit+all tests (Windows) | `cargo test` | **1334 passed**, 0 failed (722 lib unit tests; 18 in `e2e_m9`) |
| docs drift | `cargo test --lib docs`; `kelir docs --generate` | 24 passed; "nothing to generate" |
| my adversarial e2e | `cargo test --test review_m9` | **11 passed, 1 failed** (the blocker) |
| Linux gate | `bash scripts/gate.sh --linux=<machine>` (`remote-check.sh` with private work dir `kelir-verify-m9rev`) | **PASS** — `FMT-OK`, `CLIPPY-OK`, nextest `1335 run: 1335 passed (1 flaky), 3 skipped`, `DOCTEST-OK`, `remote-check: rc=0`, private work dir (never the default) |
| release build | `cargo build --release` | pass; `kelir.exe` 11 MB (< 20 MB) |

Process hygiene: `sh.exe` and `kelir.exe` counts were 0 before and after every run; every spawned
process went through `Sandbox::guard` (a Windows Job Object), every test calls `clean()`, the file
has a 100-spawn cap, and no console window was created (the harness uses `CREATE_SUSPENDED` plus
`CREATE_NO_WINDOW`). The live panes were killed by name via `hc kill` only after they had already
exited; no process was killed by image name and no process group was signalled.

## Findings

| # | sev | where | finding | failing test / proof |
|---|---|---|---|---|
| B1 | **blocker** | `src/jobrun.rs:310-312`; `src/jobs.rs:177,286-290`; `src/session.rs:1364-1368` | `jobs.toml`'s `max_steps` is parsed into `JobDef` and then **dropped**: the `CommonArgs` built for `run_as_job` sets `max_steps: args.common.max_steps.or(def.max_steps)` — but `def.max_steps` is never placed anywhere that `drive()` reads. `drive()` reads `config::load(KELIR_CONFIG)` and only applies `common.max_steps`; the machine config has no step cap. So a job declaring `max_steps = 1` runs uncapped and STATUS says `done` instead of `paused`. The CLI flag `--max-steps` works; the config-file key does not. | `tests/review_m9.rs::task_file_and_max_steps_are_honoured` FAILs: `left: "done", right: "paused"` with STATUS `{"steps":1,...,"outcome":"done"}` after one step. Verified twice. The docs advertise the key (`docs/reference/jobs.md:53`, `docs/SPEC.md:436`). Fix: write `def.max_steps` into the config the job uses (or pass it through `CommonArgs` so `drive()` applies it), and add a job-e2e that asserts `paused`. |
| S1 | should-fix | `src/jobrun.rs:274-292` (job source) vs `src/jobrun.rs:241-258` (carry source) | A rotated session gets **two** source lines before the task (a `carry` plus the `job` line). The spec says "tell it the paths with ONE source line when run under `kelir job run`". There is one job line, but the brief's "one source line" is about not growing the base prompt; two tail sources still do not touch the cached prefix, so this is a wording/interop concern, not a cache break. M9's own e2e only checks one job line on non-rotating runs. | Code reading; `tests/e2e_m9.rs::a_session_older_than_new_session_every...` asserts only the carry line. Recommend stating in `docs/reference/jobs.md` that a rotating run adds a second tail source, or merging both into one. |
| S2 | should-fix | `src/joblock.rs:59-81` (`judge(None,...)`), `src/joblock.rs:88-98` (`read`) | An unreadable lock file that is **younger than 30 s** is treated as `Fresh` — the caller returns `Busy` with a Holder whose `pid`/`cmd`/`started` are all empty. A run that skips on such a lock prints "already running" with no owner detail and appends a skipped record with `holder_pid: null`. The brief says a skip reports the holder's pid (that is explicit for names; for jobs it is implied by "never overlaps"). Not exploitable, but a degraded diagnostic. | Code reading; the existing unit test `an_unreadable_lock_is_somebodys_until_it_is_old` asserts only the verdict, not that a skip can name no one. |
| S3 | should-fix (live-drive coverage) | `scripts/live/m9_live_edge.sh` (my script) / live run | The live timeout scenario did **not** time out: the real cheap model answered in 0.8 s, so `TIMEOUT_EXIT=0` and STATUS said `done`. The concurrent-skip scenario similarly started its second run after the holder had already finished (`SKIP_EXIT=0`). The mock e2e proves both exit 75 and 124 with real processes (`tests/e2e_m9.rs`, and my `timeout_of_a_hung_request_stops_the_turn...` with a gated request), so these are product-verified, but the brief's *live* layer did not exercise them. | Live pane `live-m9edge` output quoted in the log section. A future live run needs a model that stalls (or a proxy that withholds the reply) to prove 124 against the real provider. |
| n1 | nit | `src/names.rs:22-52` | `validate()` names a leading dot or a device stem, matching the brief's `[a-z0-9._-]{1,64}` plus Windows safety. The extra "must start with a letter/digit" rule is a small deviation from a literal reading of the regex; documented in `docs/reference/jobs.md:20`. | — |
| n2 | nit | `src/names.rs:133-139` (`bind_new`), `src/names.rs:276-300` (`resolve_for_run`) | A name is bound to a fresh session id before the session starts; a crash between bind and `session_started` leaves a `new`-state name (listed by `sessions ls`). The impl notes call this intentional; it is harmless but worth a note in the reference. | — |

## Acceptance lines (independent checks)

All beyond B1 were exercised with a failing-then-passing test in `tests/review_m9.rs`:

- **named sessions**: name validation (unit in `src/names.rs`), bind/resume with `from: "job"` and a
  strict message-prefix extension (shipped e2e), `sessions ls/rename/forget` (shipped e2e), live
  collision reports the holder pid (shipped e2e), forged `owner` with a recycled start time cannot
  start a second turn (`named_session_owner_file_names_the_live_holder_and_is_pid_reuse_safe`),
  two simultaneous first runs of a free name start one session (`two_first_runs_of_one_new_name_do_not_share_a_session`).
- **jobs.toml**: `task_file` trimming and `max_steps` (B1: task_file works, max_steps is broken),
  `provider`/`model` reach the first request with the Go session header and `kelir/<version>` UA
  (`job_provider_and_model_overrides_reach_the_request`), seed-a-dead-lock takeover with a real
  loser (`no_waiter_steals_a_fresh_lock_published_during_its_second_look`), 3 real concurrent losers
  all exit 75 with one skipped record each and one model turn (`real_concurrency_exactly_one_wins_and_the_loser_is_75`),
  hung-request timeout stops the turn, exits 124, writes STATUS `timeout` and releases the lock
  (`timeout_of_a_hung_request_stops_the_turn_and_frees_the_lock`), notify reads a complete STATUS
  from `$KELIR_JOB_STATUS` (`notify_sees_a_complete_status_and_a_real_path`), notify's grandchild
  tree is reaped at its timeout (`notify_tree_kill_reaps_a_grandchild`), stopped job exits 1 with
  STATUS `stopped` (`a_stopped_job_exits_1_and_records_stopped`).
- **single-flight**: pid-reuse, ownerless, stale and fresh cases are unit-tested in `src/joblock.rs`
  and exercised through real processes above; release-only-by-token is unit-tested. `kill -9` of the
  holder frees the lock (shipped e2e `kill_9_of_the_holder...`), and a second runner's STATUS is
  not clobbered (shipped e2e and my concurrency test).
- **STATUS**: atomic writes stay parseable while a run is live
  (`status_files_are_always_parseable_while_the_run_is_live`), history and CLI reads work (shipped
  e2e), corrupt files are tolerated (shipped e2e), next-day date source without touching the cached
  prefix (shipped e2e `the_next_day_adds_the_date_source_and_nothing_else`).
- **trust**: a state-changing verb without a token is refused with no state change
  (`a_state_changing_verb_without_a_token_is_refused` on Unix; on Windows the owner-only pipe is the
  boundary, covered by the STEER-7 suite); notify env is scrubbed (shipped e2e); `names/` and
  `jobs/` hold no key or token and are owner-only (shipped e2e).
- **docs drift**: `cargo test --lib docs` passes both directions; `kelir docs --generate` reports no
  drift.

## Live-drive log

All live commands ran in Herdr panes with `KELIR_*` state dirs under
`C:/Users/the maintainer/kelir-scratch/m9rev-live`, the key sourced from `C:/Users/the maintainer/kelir-scratch/live.env`
(outside the repo; never printed), and the **release** binary.

- `live-m9g` → `bash scripts/live/m9_live.sh`, two runs of `kelir job run livedrive`:
  `RUN1_EXIT=0`, `RUN2_EXIT=0`; the second request was 90.7 % cached (`cache_hit_ratio: 0.9071`),
  one session id in both runs, `notes/live.txt` contained `M9 LIVE OK`, `sessions ls --json` showed
  one named session. Verdict **PASS** — a real cheap-model job resumes its session and keeps the
  provider cache.
- `live-m9edge` → `bash scripts/live/m9_live_edge.sh`: `SKIP_EXIT=0` and `TIMEOUT_EXIT=0` — the
  scenarios raced ahead of the holder / the model answered before the 3 s deadline, so no skip or
  timeout was observed live (S3). No product defect: the mock e2e with real processes proves those
  paths.
- Odd input / kill mid-tool / Ctrl-C / closing stdin: not driven live; covered by the existing
  M3/M4 suites and the mock. The real model produced a large answer on the first run without
  breaking STATUS (`head_of` truncates to 12 lines/1200 chars).

## What I added

- `tests/review_m9.rs` — 12 adversarial e2e tests (spawn-capped, Drop-guarded, no console windows).
  One FAILs and proves B1; eleven pass and document the rest.
- `scripts/live/m9_live.sh`, `scripts/live/m9_live_edge.sh` — live scenarios; the key is sourced
  from an env file outside the repo and never echoed. (Both scripts use `/c/...` paths because the
  Herdr pane's bash does not translate `C:/...` in a `bash <script>` argument.)

No product code in `src/` was modified.

## Spec items not yet covered

- Live 75/124 against a deliberately stalled real provider (S3); mock e2e covers them.
- The Windows Task Scheduler recipe in `docs/reference/jobs.md` was not executed (syntax from docs).
- `opencode-go` provider credential path is not live-verified (`KELIR_GO_API_KEY` unavailable);
  the mock test covers its headers only.
