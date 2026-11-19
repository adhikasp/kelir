# BUILD — build and gate speed

What a build and a gate cost on this box, what was changed to make them cheaper, and which changes were
measured in and which were rejected. Every number below was taken on the dev box (Windows 11, 12 cores, 32 GB,
`rustc 1.99.1`, `cargo 1.99.1`) with a `target/` of this checkout unless it says *cold* (no `target/` at all).
The box is shared: two or three kelir workers build at the same time, so single runs are noisy and each table
below says how many runs it averages.

## The short version

- **Gate**: run `bash scripts/gate.sh --linux`. It takes the one machine-wide lock, caps itself at 4 jobs and
  4 test threads, runs fmt, clippy, the full suite through nextest and the doctests, then Linux, and prints one
  line per stage plus a `gate: PASS`/`gate: FAIL`. Do not assemble the pieces by hand.
- **Fast loop**: `cargo test --lib <module>` for one module; `bash scripts/gate.sh --quick` for fmt + clippy
  before a commit.
- `cargo nextest run` instead of `cargo test` for the full suite: about 20 % faster and one process per test.
  `cargo test` still works; the gate just does not use it.
- `target/` is 1.4 GB for a full test build (was 3.9 GB) after the debug-info change.

## Measurements

### Windows

Cold means `rm -rf target` first; the numbers below are the ones a shared, loaded box could still resolve
(see *How these were measured*). Warm means the build after touching `src/lib.rs`, or a run with nothing
changed.

| what | before | after | how |
|---|---|---|---|
| cold `cargo build --tests` | **77.5 s** (median of 3: 69.0 / 77.5 / 91.8) | **57.5 s** (median of 3: 57.5 / 51.1 / 59.9) | `debug = "line-tables-only"` — 26 % |
| same, `target/` on disk | 3.2 GB (PDBs 1.4 GB, 86 files) | **1.4 GB** (PDBs 426 MB) | same |
| incremental test build after touching `src/lib.rs` | 12.4 s (median, n=3) | 8.9 s (median, n=12) | rust-lld (below) |
| cold `cargo build --release` (fat LTO) | 111.0 s | unchanged | untouched on purpose: shipped binaries keep fat LTO and one codegen unit |
| cold `cargo build --profile review` | n/a | 62.8 s (94.2 s under load) | `[profile.review]`: thin LTO, 16 codegen units |
| full suite, `cargo test` | 87.0 s | 84.5 s | — |
| full suite, `cargo nextest run -j 4` | n/a | 69.2 s | nextest; roughly 20 % under `cargo test` |

The `release` binary is 6.4 MB and the `review` one 7.9 MB; both are inside the 20 MB footprint budget.

### How these were measured

The dev box runs two or three kelir workers at once, so a single run proves nothing: an early A/B of the
debug-info change showed *no* difference between the arms and no difference in `target/` size either, which
is impossible — the arms had been swapped under a concurrent build. The numbers above come from an
interleaved A/B (`default`, `line-tables`, three rounds) that re-verifies after every arm that the config
it wrote is the one that was built and that `rm -rf target` actually removed the directory, and records the
resulting `target/` and PDB bytes. When you re-measure, check the size column: if both arms report the same
`target/` size, the measurement is wrong.

### The linker A/B

`rust-lld` against the default `link.exe`, measured in blocks so config swaps do not pollute the numbers
(swapping `.cargo/config.toml` invalidates every fingerprint, so each block starts with a settle build):

| linker | incremental test build, n=12 |
|---|---|
| `link.exe` | median **9.5 s** (min 8.4, max 38.8) |
| `rust-lld` | median **8.4 s** (min 7.7, max 11.8) |

About 11 % on the link plus a much tighter spread — the 38.8 s outlier is `link.exe` under load. It also
removes the Visual Studio Build Tools requirement (rust-lld ships in the toolchain), which is why it is worth
keeping even though the win is small on this box. Linux keeps its default linker: there `lld` is a separate
package, not part of the toolchain, so opting in would add an install step for no measured win.

### Linux (the maintainer, 12 cores)

| what | before | after | how |
|---|---|---|---|
| `scripts/remote-check.sh`, cold work dir | 116.9 s | — | first run in a new work dir |
| `scripts/remote-check.sh`, reused work dir | 62 s build + 29 s clippy + suite (~150 s) | **45 s**, of which 42 s is the suite | fetch + `checkout -f` into a kept clone, kept `target/` |

The suite itself dominates once the build is incremental, so the remote gate's remaining win is in the
suite: nextest took it from a serial run to `-j 4`.

## What is in the tree

| file | what it sets |
|---|---|
| `.cargo/config.toml` | `profile.dev`/`profile.test` `debug = "line-tables-only"`; `rust-lld` for `x86_64-pc-windows-msvc` |
| `Cargo.toml` | `[profile.review]` (inherits release, thin LTO, 16 codegen units) |
| `.config/nextest.toml` | gate profile: `retries = 2`, `fail-fast = false`, `test-threads = 4`, `slow-timeout` |
| `scripts/gate.sh` | the gate itself: lock, fmt, clippy, nextest, doctests, optional Linux, optional load |
| `scripts/gate-lock.sh` | one gate at a time across every worktree of this clone |
| `scripts/gate-load.sh` | the STEER-9 stress run: the suite once under N busy loops |
| `scripts/remote-check.sh` | the Linux gate into a reused work dir, plus `prune` |

`tests/buildinfra.rs` holds these as tests: the profiles cargo resolves, the nextest numbers, the lock's
behaviour (serializes, takes over a dead holder, gives up with 75, never removes a holder's lock), and the
rules the scripts must keep (nextest not `cargo test`, `--no-fail-fast`, doctests, kill by pid only).

## Knobs these scripts read

None of these is read by the `kelir` binary; they only tune the gate. They are environment variables, not
config keys, so they do not appear in `docs/reference/config.md` (STEER-11) when it lands.

| variable | default | effect |
|---|---|---|
| `KELIR_GATE_JOBS` | `4` | `CARGO_BUILD_JOBS` and nextest test threads |
| `KELIR_GATE_STRICT_RETRIES` | unset | `1` fails the gate when a test needed a retry (it is named either way) |
| `KELIR_GATE_LOCK` | `<git-common-dir>/kelir-gate.lock` | the lock path, for a driver outside the repo |
| `KELIR_GATE_TIMEOUT` | `1800` | seconds a gate waits for the lock before exiting 75 |
| `KELIR_SCRATCH` | `$HOME/kelir-scratch` | where the gate's log directories go |
| `KELIR_LINUX` | `the maintainer` | the machine `--linux` sends `remote-check.sh` to |
| `KELIR_GATE_INNER` / `KELIR_GATE_OUT` | set by `gate.sh` | internal: marks the run inside the lock and reuses its log dir |

## The rules, and why

1. **One gate at a time, machine-wide.** `scripts/gate.sh` takes `scripts/gate-lock.sh` itself, so two
   workers cannot both be in a gate by accident. The lock lives in the clone's common git dir
   (`git rev-parse --git-common-dir`), which every worktree shares — so it is one queue for every worktree on
   this machine. `KELIR_GATE_LOCK` overrides the path for a shared driver outside the repo; it must point at
   the same file so both serialize. A holder that died (a closed pane, a killed gate) is taken over after the
   waiter sees its pid is gone; a waiter that gives up exits 75 and leaves the lock alone.
2. **4 build jobs, 4 test threads.** Three workers on 12 cores: 12 jobs each is 3x oversubscription, and
   that is where the e2e flakes came from (STEER-9). The suites are green serially; at `-j 4` they stay
   green while three workers run.
3. **nextest, never as a way to hide a flake.** The gate runs `--retries 2` because a loaded box needs it, but
   it *names* every test that needed a retry, and `KELIR_GATE_STRICT_RETRIES=1` makes a retry fatal. By
   default it does not fail on one, because the flakes that remain are STEER-9's (three of them, all
   reproducible outside nextest) and a gate that is red every third run is the flake STEER-9 warns about. A
   retry is a flake to fix, not a pass — it is printed, never silent. `--no-fail-fast` so one bad suite does
   not hide the other 18.
4. **`cargo test --lib` stays the fast loop.** nextest builds the same binaries for a full run; for one module
   `cargo test --lib module::name` is still the quickest path and needs no nextest.
5. **Do not delete a work dir to make a gate pass.** `remote-check.sh prune --force` is how Linux work dirs
   are retired, and it skips anything a live run holds.

## Flakes seen from the gate

`scripts/gate.sh --load` runs the suite three times under artificial CPU load (`scripts/gate-load.sh`, N
busy loops it starts and kills itself). That is STEER-9's stress mode, and it works: 2 of 3 rounds green,
one round red.

| test | runner | how it failed |
|---|---|---|
| `e2e_m4::a_client_cannot_fabricate_a_background_job` | nextest under load | "end was refused": `LiveSession::end` got a non-zero exit from `kelir ctl <id> end` with empty stdout/stderr |
| `e2e_tui::a_ctrl_c_before_the_first_message_does_not_stop_the_turn` | nextest, four suites at once | retried: the PTY screen did not show the turn finishing in time |
| `review_steer1::a_failing_model_list_does_not_fail_the_session` | nextest on Linux | `TRY 2 PASS`, 0.07 s |
| `review_m4::r12_a_graceful_exit_leaves_a_promoted_job_running` | `cargo test`, previously | "end was refused" (`docs/QUEUE.md`) |

**This is not nextest.** Reproduced with the old runner too: five runs of `cargo test --test e2e_m4` under
six busy loops gave one "end was refused" (runs 1-4 passed, run 5 failed). `a_client_cannot_fabricate_a_background_job`
passed 8/8 under the same load in isolation and 5/5 with `cargo test`. The failure is the known `ctl end`
race on a loaded box (STEER-9, `docs/QUEUE.md:61`), not the per-process test model. Nextest only makes it
easier to *see*, because it names the retry.

**Two test groups.** `cargo test` runs one test *binary* at a time, so the PTY tests and the
process-heavy ones never used to run beside the mock-server e2e suites; nextest `-j 4` runs four at once, and
those suites are the ones that feel it. `.config/nextest.toml` caps them:

| group | filter | cap | why |
|---|---|---|---|
| `pty` | `binary(e2e_tui)` | 2 | 17 tests each driving a real terminal |
| `process-heavy` | `binary(e2e_m4) \| binary(review_m4b) \| binary(review_m4)` | 2 | each kills a session and scans for surviving pids |

Measured over repeated full runs at `-j 4`:

| config | full suite | failures |
|---|---|---|
| no groups | 66-70 s | 1 red in ~4 runs (a different test each time) |
| both groups at 2 | 87-92 s | 0 in 6 runs |
| `process-heavy` at 3 | 75-156 s | 2 of 5 runs red |
| every suite but the lib at 2 | 122-126 s | 0, but 56 s slower than needed |

The 22 s is the price of a gate that is green: the brief's point is that load flakes cost more than
compiles because they force 2-4x re-runs, and four red-in-four runs cost more wall time than 22 s each.
The underlying flakes still exist (see the table above; `review_m4b` fails 1-in-8 alone), which is STEER-9's
to fix — the groups only stop a known flake from turning a different job's gate red.

Three different tests retried in three consecutive full runs, each one reproduced outside nextest, and every
one of them is STEER-9's property, not this change's. So the gate names a retry and stays green
(`nextest: ok, but N retried test(s) above are STEER-9's to fix`), and `KELIR_GATE_STRICT_RETRIES=1` turns a
retry into a failure for a job that has fixed its own flakes. The alternative — a gate that is red every third
run because of somebody else's known flake — is exactly the harm STEER-9 describes: "a flaky gate trains
everyone to ignore failures, which hides real ones".

## Rejected or not taken

- **sccache** (cross-worktree dependency caching): not taken up here. It would help cold builds across
  worktrees, but each worker already keeps its own warm `target/`, the cold build is 45 s, and a compiler
  cache adds a server process and a config knob to every build. Revisit only if cold builds dominate again.
- **`.cargo/config.toml` `target-dir` shared across worktrees**: rejected. Two workers building into one
  directory contend on the same lock and invalidate each other's fingerprints; separate target dirs are
  faster in the parallel case and the only cost is disk.
- **Moving `tests/support` into a dev-dependency crate**: not done. It is 1.3 kLOC of test harness that every
  test binary compiles. A helper crate would make it compile once, but it would also mean a second crate in
  the workspace and a change to all 19 test files, which is more churn than the few seconds it saves.
- **`debug = 0` (no debug info at all)**: would be marginally faster but makes a panic backtrace useless; the
  whole point of the tests is that a failure is diagnosable.

## Defender exclusions

The maintainer already excluded the repo, the worktrees, scratch, `.cargo` and `.rustup` from Defender
real-time scanning; real-time protection stays on. Verified as far as an unprivileged shell can:
`(Get-MpPreference).ExclusionPath` needs administrator, so this was confirmed with the maintainer rather than
re-read. No exclusion was added or widened by this work.
