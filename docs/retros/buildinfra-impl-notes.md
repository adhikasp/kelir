# BUILD-INFRA — implementer's notes

Job: make builds and gates cheaper and calmer without changing product behaviour. Branch `job/buildinfra`,
one writer, gates on Windows and Linux. Headline: a cold test build 77.5 s → 57.5 s and `target/` 3.2 GB →
1.4 GB, the full suite 84.5 s → 69.2 s, the Linux gate 117 s → 51 s once its work dir is reused, a gate
that takes its own machine-wide lock, and a gate that still fails on a retry instead of hiding it.

## What I did, in the order the brief asked for it

1. **Gate mechanics.** `scripts/gate.sh` is now the one command: it takes `scripts/gate-lock.sh` itself, caps
   `CARGO_BUILD_JOBS=4` and nextest `-j 4`, runs fmt, clippy `-D warnings`, the whole suite, the doctests,
   and optionally the load stress run and `remote-check.sh`. `scripts/gate-lock.sh` guards every worktree of
   the clone with one lock in the shared git dir, takes over a dead holder, and a waiter that gives up exits
   75 without touching a live holder's lock. AGENTS.md's *Gates per milestone* section points at it.
2. **nextest.** Installed on this box and on the Linux box (0.9.152, the release tarball; on Windows the
   "tarball" is a zip, which cost me one failed `tar zxf`). `.config/nextest.toml` is the gate profile. The
   full suite is about 20 % faster and one process per test. `cargo test --lib` stays the fast loop. The gate
   names every retried test; `KELIR_GATE_STRICT_RETRIES=1` makes a retry fatal, and it is not the default
   (see *What was hard*: three different pre-existing flakes retried in three consecutive runs, so a strict
   default would have made every worker's gate red for somebody else's STEER-9 bug).
3. **Linux gate.** `remote-check.sh` keeps one work dir per job: fetch from the bundle, `checkout -f
   --detach`, `git clean -qfdx -e target`. Cold 117 s, reused 51 s (of which 47 s is the suite). It also
   bootstraps `~/.cargo/bin` onto PATH (a non-login `hc exec` shell has no cargo) and runs nextest when the
   box has it. `remote-check.sh prune` keeps the newest N work dirs, skips any a live run holds or that was
   written to in the last hour, and only deletes with `--force`.
4. **Defender exclusions.** Verified as far as an unprivileged shell can: `(Get-MpPreference).ExclusionPath`
   needs administrator, so this stayed a maintainer-confirmed fact, recorded in `docs/BUILD.md`. Nothing was
   added or widened.
5. **`.cargo/config.toml`.** `debug = "line-tables-only"` for dev and test; `rust-lld` for
   `x86_64-pc-windows-msvc` only.
6. **`[profile.review]`.** inherits release, thin LTO, 16 codegen units. 62.8 s cold vs 111 s, binary 7.9 MB
   vs 6.4 MB, both inside the footprint budget. A test holds release at fat LTO + 1 codegen unit.
7. **Not taken up.** sccache, a shared `target-dir`, and moving `tests/support` into a helper crate: reasons in
   `docs/BUILD.md`.

Gates on the final HEAD (`f6e3e26` and on): `scripts/gate.sh --linux` green on Windows (486 tests, 2
skipped, fmt and clippy clean) and on Linux (491 tests, 2 skipped); `scripts/gate.sh --load` 2 of 3 rounds
green with the third's failure diagnosed (below); three flash scenarios from a console-less parent with 0
visible windows and 0 leftover processes.

## What was hard

**Measuring on a box three agents share.** My first A/B of the debug-info change showed *no* difference
between the arms — and, impossibly, no difference in `target/` size either. A concurrent worker had swapped
`.cargo/config.toml` under my running build. The second attempt had its own bug: `rm -rf target` returned
before the directory was actually gone, so one arm built into the other's artifacts. The only measurements I
trust are the interleaved A/B that re-verifies the config it wrote and the resulting `target/` size after
every arm. That is now the documented method, with the size column as the tell: if both arms report the same
`target/`, the numbers are wrong. The `rust-lld` result survived the same treatment (median 8.4 s vs 9.5 s,
n=12 each, blocks so a config swap's full rebuild is not counted as a link).

**The gate found bugs in itself, twice — correctly.** The first `--load` gate run failed on my own
`waiting_gives_up...` test: nextest retried it under load, the gate named it, and the gate failed. That was
the gate working exactly as designed: the test asserted an elapsed-time ceiling (`waited < 5 s`), which is
the class of assertion STEER-9 exists to remove. Rewritten to assert what the lock promises and to wait for
an event. Then a run printed `result: no summary line` on a *green* suite: the pattern searched for
`Summary:`, but nextest prints `Summary [ 82.4s] ...` with no colon. Both are now covered by tests
(`the_gate_reports_the_summary_line_nextest_prints` greps a literal nextest line with the gate's own
pattern), so the next such drift fails a test instead of emptying a line of the gate's output.

**A test of mine leaked, and nextest said so.** `waiting_gives_up...` spawned a holder that it killed in
`Drop`; nextest's leak check (Job Object per test process) reported the race. The fix is not to suppress the
check but to let the holder end on a signal (`Held::finish`, and a `go` file it polls), with `Drop`'s
kill-by-exact-pid kept only for the panic path. A `sleep <n>` in a test is a hazard in both directions: too
short under load, wasted time when idle.

**The flakes came in waves, one per run, until I stopped treating them one at a time.** Four full runs with
retries off gave four red runs, each a *different* test (`e2e_m4::a_client_cannot_fabricate...`,
`e2e_tui::a_ctrl_c...`, `review_m4::r13...`, `review_m4b::b2...`/`b7...`). Chasing them individually was the
wrong move: each is STEER-9's, and each fix would land in a file another job owns. What they have in common is
that they kill a session, wait for process trees to die and scan for surviving pids — and `cargo test` never
ran them beside the other suites because it runs one test *binary* at a time, while nextest `-j 4` runs four.
So the fix is a scheduling one: `.config/nextest.toml` caps the PTY suite and the three process-heavy suites at
2 each. Measured: no groups 66-70 s and 1 red per ~4 runs; both groups at 2, 87-92 s and 0 red in 6 runs;
`process-heavy` at 3, 2 of 5 red; everything at 2, 122-126 s. The 22 s is cheaper than the re-runs, which is
exactly the brief's "load flakes cost more than the compiles". Three consecutive full gates (Windows and
Linux) on the final HEAD: 0 retries.

**"end was refused" is not nextest's fault.** The load stress run hit `e2e_m4::a_client_cannot_fabricate_a_
background_job` once in three. I reproduced it with the *old* runner (5 runs of `cargo test --test e2e_m4`
under six busy loops: 1 failure) and the test passes 8/8 in isolation and 5/5 under `cargo test`, so it is the
known `ctl end` race (STEER-9, `docs/QUEUE.md:61`) and the brief's warning — "`--retries` must not hide a
real failure" — cut both ways: nextest did not create the flake, it only made it visible. Recorded in
`docs/BUILD.md` rather than papered over.

**`bash` on Windows is not Git Bash.** The lock tests failed with `bash: C:Usersthe maintainer.../gate-lock.sh: No
such file or directory`: a bare `bash` on this box resolves to WSL's `bash.exe`, which rewrote the Windows
path. Fixed by resolving the shell through `kelir::config::default_shell()`, the product's own resolution, so
the test uses what kelir would use.

**No `cargo config get` / no `cargo metadata` profiles.** `cargo config get` is nightly-only and `cargo
metadata` does not report `[profile.*]`, so the test reads the same TOML a build reads and asserts the values
directly. Weaker than asking cargo, but it fails when the file changes and passes when it does not, which is
the property the gate needs.

## Where the prompt, the tools or the repo slowed me down

- **The brief's first bullet assumed the lock already existed.** "The orchestrator's gate script now holds a
  lock; document it for workers." The driver lives outside the repo (`~/kelir-scratch/gates.sh`, a `mkdir`
  lock) and is not something a worker can rely on — it only runs when the orchestrator runs it. I put the
  lock in the repo instead (`scripts/gate-lock.sh`, shared git dir), so any worker's gate serializes even
  when nobody is driving, and documented both. Neither brief nor AGENTS.md said which was wanted.
- **`scripts/remote-check.sh` was piped through `hc exec` as one inline string.** Reworking it meant the
  far-side script has to survive two levels of quoting (PowerShell-free, but `hc exec` → `bash -lc`). I wrote
  the runner to a file and `hc file write`-ed it, which is also more debuggable when it fails on the far side.
- **The lock's "who releases" bug only appears on timeout** and only from a test that actually exercises it:
  the trap removed the lock unconditionally, so a waiter that gave up deleted the *holder's* lock. That is
  the kind of bug that turns a serialized gate into two gates running at once on a bad day. Worth the four
  integration tests.
- **The dev box's file modes are `core.filemode = false`.** Asserting `100755` on the scripts in git is
  impossible here (every tracked file is `100644`), so the contract is the shebang plus the gate/test scripts
  invoking `bash <script>` explicitly. Worth knowing if a reviewer's checkout expects the executable bit.
- **`hc exec` on the Linux box starts a non-login shell**, so `cargo` was not on PATH and the first
  remote-check run failed with `cargo: command not found` after the clone succeeded. One line of PATH
  bootstrap; easy to miss because the clone and `git` worked.
- **The gate's own log directory got created twice** (outer run, then the re-exec under the lock picked a new
  timestamp). Fixed by passing it through `KELIR_GATE_OUT`; small, but it is the kind of thing that makes a
  failing gate's logs hard to find.

## For whoever picks this up

- Re-measure with `scripts/gate.sh` and the interleaved A/B method in `docs/BUILD.md`; trust the `target/`
  size column, not a single timing.
- The `ctl end` flake belongs to STEER-9. The gate will keep naming it, and that is the point.
- Nextest is now a gate dependency. `docs/BUILD.md` says how to install it on both boxes, and
  `remote-check.sh` and `gate.sh` fall back with a clear message (`nextest: MISSING`) rather than a
  confusing failure.


## Follow-up: the Linux gate leaked core dumps (buildinfra2, 2026-11-20)

The crash-injection tests (M3's `KELIR_TEST_CRASH_AT`) abort a `kelir` on purpose, and the gate box's
`kernel.core_pattern` is `systemd-coredump`: the store had grown to **2,660 cores / 904 MB, ~290 KB each**, every
one a `target/debug/kelir` SIGABRT from a gate run. A core from an intentional abort is pure waste. Fixed in
`buildinfra2`: `remote-check.sh`'s far-side runner, `gate.sh` and `gate-load.sh` set `ulimit -c 0` before the
first test binary starts (in the scripts, not on the host: no sudo, no system config, one file per property), and
`docs/BUILD.md` rule 6 plus a line in AGENTS.md's *Linux gate* keep a future editor from dropping it. Measured on
the box: the same 13 `e2e_m3` tests wrote 11 new cores without it and **0** with it (`coredumpctl` records the
aborts with `COREFILE none`). Details: `docs/retros/buildinfra2-impl-notes.md`. The lesson that generalises: the
value was invisible because nothing ever looked at the *store* — a gate's cost includes what it leaves on the box,
not only its own wall time.
