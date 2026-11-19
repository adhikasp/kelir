# BUILD-INFRA review

Reviewer: fresh QA agent. Reviewed on `main` at `997a833` (BUILD-INFRA merged as `566379c`), diff
`e13662a..HEAD -- scripts .cargo .config Cargo.toml tests/buildinfra.rs docs/BUILD.md AGENTS.md`.
No `src/` change in the diff (checked), so there is no product-behaviour or SPEC surface to review.

## Verdict: FAIL — 2 blockers, 10 should-fix

Both blockers are in the two things the brief most needed to be safe: the Linux prune deletes things it did
not create, and the gate lock can hand the lock to a second gate. Each has a failing test (below). The
rest of the job (nextest, retry reporting, `.cargo/config.toml`, `[profile.review]`, gate.sh stages) works
and is fine to keep; the fixes are small and local to `remote-check.sh` and `gate-lock.sh`.

## Evidence table

| # | sev | where | finding | proof |
|---|---|---|---|---|
| B1 | **blocker** | `scripts/remote-check.sh:48-52,72` | `prune` removes **any** `$HOME/kelir-verify*` entry by name pattern alone. No marker file, no ownership check. Three ways to delete what it did not create (below). | `tests/buildinfra_review.rs` `prune_only_removes_directories_it_created`, `prune_job_cannot_name_a_path`, `prune_rejects_a_keep_that_is_not_a_number` (all FAIL), plus a run on the Linux box |
| B2 | **blocker** | `scripts/gate-lock.sh:62,71-75` | A waiter that has itself waited >= 10 s treats **any** ownerless lock as stale, including one a new holder created milliseconds ago and has not yet written its `owner` file into (`mkdir`, then a `date` fork, then the write). The new holder's lock is `mv`ed away: two gates run at once. Reproduced with real concurrency, not only with an enlarged window. | 1 of 4 natural runs of 5 concurrent gates: `max concurrent: 2` and `gate-lock: removed a stale lock (the lock has no owner file)`; `a_waiter_does_not_steal_a_lock_that_was_just_created` (FAILs) |
| S1 | should-fix | `tests/buildinfra.rs:89-90` | The "release stays fat LTO" pin is not real: it is `str::contains` on raw text. `codegen-units = 16` contains `codegen-units = 1`; a comment contains `lto = true`. Mutated `Cargo.toml` (release `codegen-units = 16`; release `lto = "thin" # was lto = true`): the test still **passes** both times. | mutation runs; `release_is_pinned_by_value_not_by_substring` (passes today, parses the values) |
| S2 | should-fix | `scripts/gate-lock.sh:71-75`, `docs/BUILD.md` "rules" 1 | The documented interop with an outside driver does not work. `~/kelir-scratch/gates.sh` locks with a plain `mkdir` (no `owner`). Point `KELIR_GATE_LOCK` at that path as the doc says and a gate walks into it after 10 s while the driver is mid-gate. | ran it: `removed a stale lock (the lock has no owner file)`, command ran after 11 s |
| S3 | should-fix | `scripts/gate-lock.sh:97-101` | The lock is tied to the wrapper, not the command tree. `kill -9` of the wrapper: the lock is "stale (pid gone)" while the orphaned gate keeps running, and the next gate starts on top of it. `kill -TERM` releases the lock while the child (`sleep`) is still alive. | `kill -9 H`: log shows `E HOLD ... E NEXT ... X NEXT ... X HOLD` (NEXT ran inside HOLD); TERM: lock gone, `sleep 20` still running |
| S4 | should-fix | `scripts/gate-lock.sh:52,79` | Two TOCTOU holes in takeover, not reproduced but visible in the code. (a) A waiter that observed a stale lock `mv`s whatever is at `$lock` *now*, which may be a lock another waiter just legitimately took. (b) `release` runs `rm -rf "$lock"` without checking it still holds it, so a holder whose lock was stolen (B2, S3) deletes the new holder's lock and lets a third gate in. | code reading |
| S5 | should-fix | `.config/nextest.toml`, `scripts/gate.sh:103` | AGENTS.md's no-leaked-processes rule is a hard rule, but a leaking test **passes the gate**: nextest prints `LEAK ... Summary ... 1 passed (1 leaky)`, exit 0, and gate.sh only greps `TRY`/`FLAKY`. `result:` shows `(1 leaky)` in passing, nothing names the test. Add `leak-timeout = { period = "100ms", result = "fail" }` (nextest >= 0.9.9x) or grep `LEAK`. | `probe_leaks_a_child` in `tests/buildinfra_probe.rs`: `LEAK [0.234s] ... (1 leaky)`, exit 0, child still alive |
| S6 | should-fix | `scripts/remote-check.sh:128-157` | Far-side logs are fixed paths in `/tmp` (`fmt.log`, `clippy.log`, `nx.log`, `ct.log`, `doc.log`). Two Linux gates for different jobs at once (the box is shared; another worker's `kelir-verify-tui2m` run was in flight during this review) overwrite and grep each other's results: job A can report job B's pass or fail. Use `$wd/logs/`. | code reading; concurrent runs observed |
| S7 | should-fix | `AGENTS.md:60-65`, `scripts/remote-check.sh:85` | AGENTS.md "Linux gate" still says `scripts/remote-check.sh <machine>`. With no work dir name that is the default `kelir-verify`, now a **persistent shared clone**: every worker who follows AGENTS.md literally runs `checkout -f` + `git clean -fdx` in the same tree and overwrites the same `lock` file (no mutual exclusion). Only the "Parallel work" section and `gate.sh --linux` pass `kelir-verify-<job>`. Also: the script bundles committed `HEAD` only and says nothing when the working tree is dirty, so a green remote gate can describe different code than the one on disk (my own uncommitted test files were not in the Linux run). | AGENTS.md text; Linux run with uncommitted files was green |
| S8 | should-fix | `scripts/remote-check.sh:48-77` | Spec says "keep the last 2-3 work dirs **per active job**". `prune` without `--job` keeps the newest N **overall**: an active job idle for an hour loses its warm `target/` (a cold 62+29 s build) while a finished one is kept. On the real box the dry run kept `kelir-verify-kelir`, `-buildinfra`, `-tui2fix` and marked 25 other jobs stale. | dry run on the Linux box |
| S9 | should-fix | `scripts/gate.sh:6,36`, `docs/BUILD.md:65,103` | **Hardcoded private host name `the maintainer` as the default Linux machine** in a repo that will be public (AGENTS.md: "no hostnames"). Also in docs/QUEUE.md and ~20 older docs/reviews/retros (pre-existing, not part of this diff). Make `--linux` require `KELIR_LINUX` / `--linux=MACHINE` and say "the Linux machine" in docs. | grep |
| S10 | should-fix | `docs/BUILD.md:36` vs `:160-161`; install | (a) The headline suite number `69.2 s` is the "no groups" config; the shipped config is `87-92 s` by the same doc's own table. (b) The impl notes say BUILD.md explains how to install nextest on both boxes; it does not (no install section: `grep -n -i install docs/BUILD.md` finds one unrelated line). A worker without nextest gets `nextest: MISSING — install it (docs/BUILD.md)` and nothing there. | grep |
| S11 | should-fix | `scripts/gate.sh:99-121` | On a failing nextest the gate prints `nextest: FAILED — <log>` and nothing else: no `result:` line, no names of the failed tests (they are only in the log). On success with one flaky test it prints `2 retried test(s)` (the `FLAKY` and the `TRY 2` line of the same test are both counted). | probe runs below |
| n1 | nit | `scripts/gate-lock.sh:70` | A holder pid that is reused by an unrelated process makes the lock look live until `KELIR_GATE_TIMEOUT` (1800 s), then exit 75. Safe side, but there is no start-time check, and the failure mode is a 30 min stall. | code |
| n2 | nit | `scripts/remote-check.sh:54,61,63` | `stat` follows a symlink, so a symlink named `kelir-verify-x` takes a "keep" slot by its **target's** mtime; if pruned, `rm -rf` removes the link (not the target; verified). Directory mtime is a weak "in use" signal: it only moves when entries are added to the top level. | decoy run |
| n3 | nit | `scripts/remote-check.sh:85,111` | The work-dir name is unvalidated (`W=../x`, `W=.`), and `gate.sh` derives it from the checkout's basename, so two checkouts called `kelir` collide. Not a deletion path (only `src-tree` under it is ever `rm -rf`ed). | code |
| n4 | nit | `tests/buildinfra.rs` | Lock tests use `sleep 3`-style holders; fine, but none exercises the ownerless-fresh-lock path (B2) or a non-`owner` driver (S2). The new `buildinfra_review.rs` does. | — |

## 1. prune safety (the focus)

Method: extracted the far-side script **verbatim** from `remote-check.sh` and ran it on the Linux box against
a throwaway `HOME` (`~/qa-prune-fake`, removed afterwards; the real `$HOME` was only dry-run) with decoys:
`kelir-verify-notes` (a user project with `.git` and a README), `kelir-verifyXYZ`, a symlink
`kelir-verify-link -> precious/`, a plain file `kelir-verify-file`, a "busy" dir whose `lock` names a live
pid, five genuine-looking work dirs, a sibling `Documents/` and `precious/`.

- **Dry run** (no `--force`): deletes nothing. Good. `--force` is the only switch; the dry run lists exactly
  what the forced run removes (matches).
- **`--keep 3 --force`**: removed `kelir-verify-notes` (the user project) and `kelir-verifyXYZ` along with
  the genuine dirs. **Name pattern is the only test.** There is no marker file, so "directories it created"
  is not what the code checks. (B1)
- **`--keep abc --force`**: `[: abc: integer expected` on stderr, exit 0, and `[ $n -le abc ]` is false, so
  **every** non-busy dir was removed, again including the user project. (B1)
- **`--job 'alpha/../Documents' --force --keep 0`**: removed `~/Documents` (a sibling of the work dirs; the
  path is `"$HOME/kelir-verify-$job"`). `--job '*'` expands as a glob. The job name is spliced into a path
  with no validation. (B1)
- **Symlink**: `rm -rf` on a symlink removes only the link; the target survived (`precious` intact). Not
  followed. But it is still deleting something that is not a work dir. (n2)
- **Busy dir** (live pid in `lock`) and **recently written** dirs are skipped. Good. A plain file is skipped
  by `-d`. Good.
- **Real box, dry run only**: 28 `kelir-verify*` dirs (~100 GB); keeps 3, lists 25 as stale. I did not run
  `--force` against the real `$HOME`.

Fix sketch: `remote-check.sh` writes `.kelir-workdir` (with the job name and creation time) in `$wd` when it
creates it; `prune` considers only entries that are real directories (`[ -d ] && [ ! -L ]`), match
`kelir-verify[-A-Za-z0-9_.]*` with no `/`, **and** contain the marker; validates `--keep` with
`case $keep in ''|*[!0-9]*) exit 2;; esac` and `--job` against `^[A-Za-z0-9._-]+$` (no `..`); group by job
to honour "per active job". Remove the `ignore` on the three prune tests when it lands.

## 2. gate-lock

Real concurrency, 5 gates started in the same instant (`KELIR_GATE_LOCK` in a temp dir, the critical
section logs enter/exit timestamps and the script computes max overlap):

| run | hold | max concurrent | notes |
|---|---|---|---|
| 1, 2 | 12 s | 1 | clean |
| 3, 4, 6 | 11 s | 1 | clean |
| 5 | 11 s | **2** | `removed a stale lock (the lock has no owner file)` — a live gate was stolen |

So exclusion holds most of the time and fails exactly where the code says it can (B2). Deterministic
reproduction (`a_waiter_does_not_steal_a_lock_that_was_just_created`, 15 s): a waiter that has waited 12 s;
a new holder `mkdir`s the lock and takes 3 s to write `owner`; the waiter prints `STOLEN`.

Other lock properties, tested:

- **Stale holder takeover** (dead pid): taken over with a note, command runs. OK.
- **Live holder is not stolen** while it has an `owner` file with a live pid: waiter exit **75**, holder's
  lock directory intact afterwards. OK (the `release only by holder` fix works for the timeout path).
- **Crash mid-gate**: `kill -9` of the wrapper leaves a stale lock that the next gate clears (no permanent
  lock), but the *command* of the dead wrapper was still running and overlapped the next gate (S3).
- **SIGTERM**: `EXIT` trap runs, lock removed — while the child is still alive (S3).
- **pid reuse**: not testable deterministically; by code a reused pid makes the lock look held (safe side,
  30 min stall; n1). It cannot make a live gate look dead except through B2/S3.
- **Outside driver**: S2.

## 3. nextest, retries, leaks, windows

Through `scripts/gate.sh -- --run-ignored only -E '...'` with probes in `tests/buildinfra_probe.rs`:

| probe | result |
|---|---|
| flaky (fails attempt 1, passes 2) | `result: ... 1 passed (1 flaky)`, `RETRIED (a flake to fix, STEER-9):` lists `FLAKY 2/3 ... probe_flaky_fails_first_attempt_only` and the `TRY 2 PASS` line; `nextest: ok, but 2 retried test(s)...`; **gate: PASS**, exit 0 |
| same with `KELIR_GATE_STRICT_RETRIES=1` | `nextest: FAILED (...)`, **gate: FAIL**, exit 1 |
| always failing | `nextest: FAILED — <log>`, `gate: FAIL`, exit 1 (but the test is not named in the gate output; S11) |
| leaks a child | nextest: `LEAK`, `1 passed (1 leaky)`, exit 0, **gate would PASS** (S5) |

So `--retries 2` does not hide a failure (a permanent failure fails; a retried one is named and can be made
fatal), with the wording nits in S11. The default being "named but green" is a documented, reasoned choice;
I do not flag it, but note the real Linux gate printed `FLAKY 2/3 ... review_m3 r8_resume_continues_a_stopped_turn`
and still `rc=0` (that test passed on the second Linux run, so it is flaky, not fixed).

Full-run process/window checks (Windows):

- Full suite from `gate.sh` (lock taken, fmt, clippy, nextest, doctest): **641 passed, 2 skipped, 0 retried,
  `gate: PASS`**. PID snapshots before/after: **0 `kelir.exe` afterwards** (8 other workers' before); the
  only new `bash.exe` were another worker's (`remote-check.sh the maintainer kelir-verify-tui2m`, a tui2m session
  command) with live parents outside my tree; orphan scan of kelir/bash/cargo/rustc/sleep created in the
  last 12 minutes found exactly that one worker's tree, none of mine.
- Full `cargo nextest run -j 4` started from a **console-less parent** (`launch_detached.py detached`,
  `winspy` watching, 641 passed, 102 s): 22 `WIN CREATE` events, **0 visible** (`vis=1`: 0); the
  `PseudoConsoleWindow`s are the `e2e_tui` PTYs and `OpenConsole`/`Windows Terminal` hidden hosts. 0 leftover
  `kelir.exe`.

## 4. `.cargo/config.toml`

`cargo build --tests -v` shows `linker=rust-lld` on Windows. A panic with `RUST_BACKTRACE=1` still prints
`tests\buildinfra_probe.rs:23` and `.\tests\buildinfra_probe.rs:23` frames with
`line-tables-only` — file:line works, which is what a failure needs. Variable inspection in a debugger is
gone (the doc says so). Linux: no `[target.*linux*]` section, and the Linux gate (fmt, clippy, 641 tests)
passes on the default linker. A clean build and the full suite pass. OK.

## 5. `[profile.review]`

Exists (`inherits = "release"`, thin LTO, 16 CGUs); `cargo build --profile review` works: **70 s cold**
(doc: 62.8 s), binary 8.2 MB (doc: 7.9 MB), within the 20 MB budget. Release still `lto = true`,
`codegen-units = 1`, `strip = true` in the manifest, but the pinning test cannot catch a change (S1).

## 6. Numbers re-measured (Windows, this box, loaded: other workers building/testing, CPU 67-100 %)

| claim in docs/BUILD.md | I measured | reproduces? |
|---|---|---|
| cold `cargo build --tests`: 57.5 s | 73 s (default jobs), 149 s (`CARGO_BUILD_JOBS=4`, CPU at 100 %) | direction yes (`target/` 1.6 GB vs "1.4 GB"), absolute no; box was busier |
| full suite, nextest `-j 4`: 69.2 s (87-92 s with the groups) | 102 s (detached, `-j 4`), 126 s (`gate.sh`, `-j 4`) | no; 12-40 % over the doc's own grouped figure, same ordering. The 69.2 headline is the ungrouped config (S10) |
| cold `--profile review`: 62.8 s | 70 s | yes |
| Linux suite, reused dir: 42 s | `Summary [ 64.4s ]` and `[ 63.8s ]` (twice) | no (the box was also running another worker's gate) |
| Linux reused work dir gate faster than cold | ~yes (runner warm, `target` kept, second run fast) | yes |

I treat the load as the cause of the gap, not a defect, but the doc's table should carry the load
(`cat /proc/loadavg`, CPU %) next to the numbers.

## 7. Hard-coded host name

`the maintainer`: `scripts/gate.sh:6` (comment), `scripts/gate.sh:36` (the actual default), `docs/BUILD.md:65,103`
(S9). `scripts/remote-check.sh` itself is clean (machine is an argument). No other personal data in the
diff: no usernames, paths under `/home/` or `C:/Users`. (The pre-existing docs outside this diff mention the
host too.)

## 8. AGENTS.md

"Gates per milestone" now names `bash scripts/gate.sh [--linux]`, `--quick`, `cargo test --lib <module>`,
the lock, `--no-lock`, `KELIR_GATE_STRICT_RETRIES`: correct. Gaps: S7 (Linux gate section names the old
invocation with the shared default dir; no word about `prune` or about committing before the remote check);
nextest is a dependency of the gate but AGENTS.md/BUILD.md do not say how to install it (S10).

## Pyramid

- `cargo fmt --check`: clean (after my own files were formatted); `cargo clippy --all-targets -- -D warnings`: clean.
- Windows: `scripts/gate.sh` **641 passed, 2 skipped, 0 retried**, doctest ok; plus one detached
  `cargo nextest run -j 4`: 641 passed, 5 skipped (the 2 pre-existing ignores plus my 3 probes at that
  time). With my files added, `main` has 1 more passing test and 7 more `#[ignore]`d (4 review + 3 probe).
- Linux: `bash scripts/remote-check.sh the maintainer kelir-verify-buildinfra-rev` twice on `997a833`: fmt ok,
  clippy ok, **641 passed, 2 skipped** (run 1 had `1 flaky`: `review_m3 r8_resume_continues_a_stopped_turn`
  passed on try 2/3; run 2 had 0 retries), doctest ok. Not run on the Linux box: my uncommitted test files
  (the script bundles `HEAD`, S7); they use `cfg(windows)` only around `CREATE_NO_WINDOW` and compile on
  both.
- Live drive (OpenRouter, Herdr panes): **not applicable** — the job has no product behaviour and `src/`
  is untouched. The equivalents (full-suite leak/window checks from a console-less parent, flaky/failing/
  leaking probes through the real gate, 5-way real lock contention, prune against decoys on the real Linux
  box) were run above.

## What I added

- `tests/buildinfra_review.rs`: 1 passing test (strict release pin, S1 fix) and 4 `#[ignore]`d tests that
  FAIL on the reviewed code and prove B1 (three) and B2 (one). Remove the `ignore` in the fixing commit; they
  are ignored only so the shared `main` stays green for other workers meanwhile.
  `cargo nextest run --run-ignored only -E 'binary(buildinfra_review)'`.
- `tests/buildinfra_probe.rs`: three `#[ignore]`d probes (flaky, always failing, leaking) used for §3.

No product code and no scripts were changed. Cleaned up: `~/qa-*` files and the fake home on the Linux box,
my `lk/` scratch locks, the probe child (exited by itself after 20 s).

## Spec / brief items not met

| brief line | status |
|---|---|
| 1 gate mechanics (one gate at a time) | mostly: B2, S2, S3 |
| 2 nextest, `--retries` must not hide a failure | met; leaks not enforced (S5) |
| 3 Linux gate reused dir + prune "per active job", never deleting others' dirs | reuse met; prune unsafe (B1), not per job (S8) |
| 4 Defender exclusions: verify, do not widen | `Get-MpPreference` needs admin (`N/A: Must be an administrator`), matches the doc; unverifiable here |
| 5 config.toml | met |
| 6 `[profile.review]` + fat LTO release kept | met; pin test ineffective (S1) |
| 7 sccache / helper crate | deliberately not done, reasons documented |
