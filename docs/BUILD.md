# BUILD — build and gate speed

What a build and a gate cost on this box, what was changed to make them cheaper, and which changes were
measured in and which were rejected. Every number below was taken on the dev box (Windows 11, 12 cores, 32 GB,
`rustc 1.99.1`, `cargo 1.99.1`) with a `target/` of this checkout unless it says *cold* (no `target/` at all).
The box is shared: two or three kelir workers build at the same time, so single runs are noisy and each table
below says how many runs it averages.

## The short version

- **Gate**: run `bash scripts/gate.sh --linux=<machine>`. It takes the one machine-wide lock, caps itself at
  4 jobs and 4 test threads, runs fmt, clippy, the full suite through nextest and the doctests, then Linux,
  and prints one line per stage plus a `gate: PASS`/`gate: FAIL`. Do not assemble the pieces by hand. The
  Linux machine is named on the command line (or in `KELIR_LINUX`); it is never a default in the repo.
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
| full suite, `cargo nextest run -j 4` | n/a | 66-70 s ungrouped, **87-92 s as shipped** | nextest; the shipped config is the grouped one below, so 87-92 s is the number a gate pays |

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

### Linux (the far box, 12 cores)

| what | before | after | how |
|---|---|---|---|
| `scripts/remote-check.sh`, cold work dir | 116.9 s | — | first run in a new work dir |
| `scripts/remote-check.sh`, reused work dir | 62 s build + 29 s clippy + suite (~150 s) | **45 s**, of which 42 s is the suite | fetch + `checkout -f` into a kept clone, kept `target/` |

The suite itself dominates once the build is incremental, so the remote gate's remaining win is in the
suite: nextest took it from a serial run to `-j 4`.

## Installing what the gate needs

The gate needs `cargo-nextest` on both boxes; without it, `scripts/gate.sh` fails the run and says so. Install
it with cargo (versions in use when this was written: `cargo-nextest 0.9.152`, `rustc 1.99.1`):

```bash
cargo install cargo-nextest --locked
cargo nextest --version          # 0.9.x or newer: `leak-timeout result = "fail"` needs >= 0.9.9x
```

On the Linux box the same command works (a login shell has `~/.cargo/bin` on `PATH`; `remote-check.sh` adds it
when it does not). `cargo test` still works without nextest, but the gate will not: nextest is what runs each
test in its own process, names a retry and enforces the leak rule. Nothing else in the gate needs installing
(`rust-lld` ships with the toolchain).

## What is in the tree

| file | what it sets |
|---|---|
| `Cargo.toml` | `[profile.dev]`/`[profile.test]` `debug = "line-tables-only"` (the project default; ships with the source); `[profile.review]` (inherits release, thin LTO, 16 codegen units) |
| `.cargo/config.toml` | `rust-lld` for `x86_64-pc-windows-msvc` only; no `[profile]` overrides (those would shadow the manifest default above) |
| `.config/nextest.toml` | gate profile: `retries = 2`, `fail-fast = false`, `test-threads = 4`, `slow-timeout`, and `leak-timeout` with `result = "fail"` so a leaked process is a failure |
| `scripts/gate.sh` | the gate itself: lock, fmt, clippy, nextest, doctests, optional Linux, optional load |
| `scripts/gate-lock.sh` | one gate at a time across every worktree of this clone (atomic publish; waits on an outside driver's lock) |
| `scripts/gate-load.sh` | the STEER-9 stress run: the suite once under N busy loops |
| `scripts/remote-check.sh` | the Linux gate into a reused work dir (with `ulimit -c 0`), plus marker-guarded, per-job `prune` |

`tests/buildinfra.rs` holds these as tests: the profiles cargo resolves, the nextest numbers, the lock's
behaviour (serializes, takes over a dead holder, gives up with 75, never removes a holder's lock, never takes
over a lock it did not write), and the rules the scripts must keep (nextest not `cargo test`, `--no-fail-fast`,
doctests, kill by pid only). `tests/buildinfra_review.rs` is the permanent proof of the review's findings:
prune removes only dirs it created (decoys: a user project with the same name, a bare `mkdir`, a symlink, a
dir with a foreign file, a nested dir), keeps per job, is a dry run without `--yes`; five gates at once run
strictly one at a time; a wrapper killed with `-9` does not hand the lock over while its command runs; a
reused pid is stale and a live one is not; an outside driver's `mkdir` lock is respected; a leak fails the
gate and a failing run names its tests. The core-dump rule is held by tests too: the far side
that actually runs is printed by `scripts/remote-check.sh far-env` (and is the top of `far-runner`), and a
test executes that exact text and asserts a child of it prints `ulimit -c = 0` — so the prologue cannot be
dropped, moved below the first test binary, or split into two copies that disagree.

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
| `KELIR_LINUX` | unset | the machine `--linux` sends `remote-check.sh` to; with neither the flag nor this, the gate says how to set it and exits 2 |
| `KELIR_GATE_INNER` / `KELIR_GATE_OUT` | set by `gate.sh` | internal: marks the run inside the lock and reuses its log dir |

## The rules, and why

1. **One gate at a time, machine-wide.** `scripts/gate.sh` takes `scripts/gate-lock.sh` itself, so two
   workers cannot both be in a gate by accident. The lock lives in the clone's common git dir
   (`git rev-parse --git-common-dir`), which every worktree shares — so it is one queue for every worktree on
   this machine. `KELIR_GATE_LOCK` overrides the path for a shared driver outside the repo; it must point at
   the same file so both serialize.
   The lock is a *file* whose content is the holder's record (token, pid, start time, command line), published
   with `ln -T` so it is create-or-fail: a lock always names its owner and two gates can never both publish.
   The gate runs its command in a process group of its own and the command's side records that group, so a
   wrapper killed with `-9` does not free the lock while its command is still running; the next gate waits for
   the group. A holder is taken over only when its pid is provably gone (or a *different* process has the pid:
   the start time is part of the record) and its group is empty, re-checked after the lock has been moved aside
   atomically. A holder only removes a lock that still carries its own token.
   A lock this script did not write is **not** taken over: an outside driver that locks with a plain `mkdir`
   is waited on (the script says so, and says how to clear it by hand), because the cost of a wrong "stale"
   verdict is two gates at once. A waiter that gives up exits 75 and leaves the lock alone.
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
5. **Do not delete a work dir to make a gate pass.** `remote-check.sh prune` is how Linux work dirs are
   retired, and it is deliberately paranoid: it deletes only directories *it created*, proven by an on-disk
   marker written when the dir is made; the name must be exactly `kelir-verify-<plain job name>`; the dir must
   not be a symlink and its real path must be directly under `$HOME`; it must contain nothing but what a work
   dir contains; it must not be a live run (`lock` names a running pid) and not have been written to in the
   last hour. Everything else is reported as `skip` with the reason and left alone. Keeping is the newest
   `--keep` (default 3) **per job**, so an idle job keeps its warm `target/` no matter how many other jobs ran
   after it. Nothing is removed without `--yes`: the first call is always a dry run.
6. **No core dumps: `ulimit -c 0` is deliberate — do not drop it.** The suite contains tests that abort a
   `kelir` on purpose (M3's `KELIR_TEST_CRASH_AT` crash injection, and the reviews that re-run it). On a stock
   Linux box each one wrote a core into the system store: on 2026-11-20 the gate machine held **2,660 cores,
   904 MB, ~290 KB each**, every one a `.../target/debug/kelir` SIGABRT from the Linux gate. Whether a core is
   written is the host's decision (`kernel.core_pattern` here is `systemd-coredump`, plus `fs.suid_dumpable`),
   not the repo's, so the limit is set where the tests are started and inherited by every child: `gate.sh`
   sets it in its Unix branch before the first stage, `scripts/gate-load.sh` sets it again for a direct run,
   and `remote-check.sh` sets it in the far-side runner **before the first test binary starts**, printing the
   live `ulimit -c` so the gate log says it is 0. It is in the scripts, not on the host, on purpose: no sudo,
   no system configuration touched, and the same source file governs every box the gate runs on. A core from
   an intentional abort is pure waste, and 904 MB of it is waste that fills the disk.

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
| `pty` | `test(/^tui::/)` | 2 | the tests driving a real terminal |
| `process-heavy` | `test(/^bg::/)` and the process-heavy gate/review modules | 2 | tests that kill a session and scan for surviving pids |

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

- **sccache** (compiler cache): available as an opt-in, not a default. It cannot cache incremental
  builds, so it only pays when `CARGO_INCREMENTAL=0`; with a warm `target/` that trade is a loss,
  but across fresh worktrees it avoids recompiling unchanged dependencies. Measured on this box
  (Linux, 12 threads): a cold test build with the wrapper took 43 s; touching `src/lib.rs` and
  rebuilding took 41 s without sccache and 16-37 s with it (one cache hit for the crate that
  changed, its dependents rebuilt). The wrapper is `scripts/rustc-wrapper.sh`:
  `export RUSTC_WRAPPER="$(pwd)/scripts/rustc-wrapper.sh" CARGO_INCREMENTAL=0` before a build.
  It forwards to rustc when sccache is not installed, so enabling it by accident is harmless.
  Not wired into `.cargo/config.toml` on purpose: a relative path there breaks clones whose
  absolute path differs, and CI installs sccache separately if it wants it.
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
