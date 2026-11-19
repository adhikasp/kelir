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
   it *names* every test that needed a retry and fails the gate unless `KELIR_GATE_ALLOW_RETRIES=1`. A retry
   is a flake to fix, not a pass. `--no-fail-fast` so one bad suite does not hide the other 18.
4. **`cargo test --lib` stays the fast loop.** nextest builds the same binaries for a full run; for one module
   `cargo test --lib module::name` is still the quickest path and needs no nextest.
5. **Do not delete a work dir to make a gate pass.** `remote-check.sh prune --force` is how Linux work dirs
   are retired, and it skips anything a live run holds.

## Retries seen in the wild

The Linux gate reported one flaky test on its first nextest run:

- `review_steer1::a_failing_model_list_does_not_fail_the_session` — `TRY 2 PASS`, 0.07 s. A timing
  assumption in that test, not a product bug; it is STEER-9's territory (event-driven waits) and not fixed
  here. This is exactly the case rule 3 exists for: it is named, not silently retried.

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
