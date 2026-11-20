# buildinfra2 — implementer's notes

Job: make the Linux gate stop leaking core dumps. One finding
(`docs/tasks/BUILD-INFRA-coredump.md`), no product code touched, one commit.

## What I did

1. **`scripts/remote-check.sh` — the far side.** The runner the gate writes to the Linux box now opens with a
   prologue that sets `ulimit -c 0` and prints the live value (`CORE-LIMIT: ulimit -c = 0`). The limit is set
   in the shell that starts the stages, before `cargo fmt`, so every child — nextest, each test binary, the
   `kelir` a test aborts — inherits it. No `-S`/`-H`: this lowers the soft *and* the hard limit, so nothing the
   gate runs can raise it back. It is in the script, not on the host: no sudo, no system config touched, and
   the same file governs every Linux box the gate runs on.
2. **The runner is now readable text, not a heredoc inside the script's flow.** `remote-check.sh far-runner`
   prints exactly what is shipped, `far-env` prints the prologue on its own, `far-argv` prints the far command
   as argv. That is what makes the property testable: a test runs the *shipped* text and asks a child of it
   what its core limit is, instead of grepping the script for a string that may sit in a comment.
3. **`far-argv` (instead of an `env W=… EXTRA="${extra[*]}"` string).** The far command is read into an array
   on this side, so an extra nextest argument with a space in it arrives as one argument. Small correctness win
   found while making the command testable; the old form split it on the far side.
4. **`scripts/gate.sh` and `scripts/gate-load.sh`.** The same `ulimit -c 0` in a `Linux | Darwin` branch, so a
   gate run on Linux/macOS, and a direct run of the stress script, cannot dump either. Windows is untouched (no
   `ulimit` there; the branch keys off `uname -s`).
5. **Docs.** `docs/BUILD.md` rule 6 with the numbers (2,660 cores / 904 MB / ~290 KB each on 2026-11-20, all
   `target/debug/kelir` SIGABRT), why the limit is in the scripts and not on the host, and a line in the tests
   paragraph saying how it is proven. One paragraph in AGENTS.md's *Linux gate* section.
6. **Tests** (`tests/buildinfra.rs`): `the_linux_gate_runs_with_core_dumps_off` runs `far-env`, asserts its text
   is the head of `far-runner` (one copy, not two that can drift), that it precedes `git clone`, `cargo fmt`,
   nextest and the doctests, and then **executes that text** and requires
   `CORE-LIMIT: ulimit -c = 0` from inside it. `the_local_gate_scripts_also_turn_core_dumps_off` holds
   `gate.sh`/`gate-load.sh` to the same rule and fails if anything ever says `ulimit -c unlimited`. Both fail
   when the line is removed (checked by deleting it and running the test).

## Verification on the Linux box (read-only, no sudo)

`coredumpctl` and `/var/lib/systemd/coredump` are world-readable here, so this could be measured directly
rather than argued:

| run on the gate box | dumps stored | store |
|---|---|---|
| `e2e_m3` with no prologue (control) | 11 new, ~290 KB each | 2,675 → 2,686 files, 908 → 911 MB |
| `e2e_m3` under `eval "$(bash scripts/remote-check.sh far-env)"` | **0** (`coredumpctl` lists the 11 SIGABRTs with `COREFILE none`) | 2,686 files / 911 MB before **and** after |

"Abort recorded, core not written" is exactly the state we want: the crash still happens (the test needs it),
the store does not grow. The gate itself agrees: the `bash scripts/gate.sh --linux=<machine>` run's log line 1 is
`CORE-LIMIT: ulimit -c = 0`, and every `kelir-verify-buildinfra2` entry it produces is `none`.

Full gate at the commit below: Windows fmt/clippy/nextest (`708 tests run: 708 passed, 5 skipped`, no retries)
and doctests green; the Linux half `FMT-OK CLIPPY-OK NEXTEST-OK (708 passed) DOCTEST-OK`, `gate: PASS`.

## What was hard / worth knowing

- **The prologue had to be testable, and the first version of "testable" was a lie.** Grepping the script for
  `ulimit -c 0` passes on a comment, and it cannot tell whether the line runs *before* the first test binary —
  the only thing that matters. Shipping the text through `far-env`/`far-runner`/`far-argv` turned the script
  into something a test can execute, and the "before every stage" assertion then also protects the far side's
  ordering. The `far_prune` review finding (`the_prune_body_survives_its_own_quoting`) is the same lesson from
  the other direction: script text that is never executed by a test rots silently.
- **Where to put the limit.** The host's `kernel.core_pattern` is `systemd-coredump`, and `fs.suid_dumpable` /
  the pipe handler decide whether a core is stored — so a repo-only fix (e.g. "don't abort") was not available,
  and a host setting would need sudo and would not travel with the gate. Setting it in the shell that starts
  the tests is the one place that is both in-repo and inherited by everything.
- **Don't over-claim.** The limit applies to what the gate starts. It does not clean up the 900 MB already in
  the store (that is the maintainer's call, needs root, and the brief said read-only), and it does not stop
  anything outside the gate from dumping.
- `bash scripts/remote-check.sh far-env` is now part of the script's contract, so `usage()` prints the whole
  header (awk up to the first code line) instead of a fixed line range — a fixed `sed -n '2,40p'` silently
  truncated the header the moment it grew, and the header is where the subcommands are documented.
