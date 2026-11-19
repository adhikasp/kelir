# BUILD-INFRA fixes — implementation notes

Brief: `docs/tasks/BUILD-INFRA-fixes.md`; findings: `docs/reviews/buildinfra-review.md`
(FAIL — 2 blockers, 10 should-fix, 4 nits). All of them are addressed; the two blockers and every
should-fix has a test that fails on the reviewed code, in `tests/buildinfra_review.rs`.

## The two blockers

### B1 — `prune` deleted what it did not create

`prune` matched `$HOME/kelir-verify*` by name and `rm -rf`ed it. Now the far side proves ownership
before it decides anything:

- the work dir carries a marker, `.kelir-workdir` (`token`, `created`, `job`, `creator`), written by
  `remote-check.sh` **only when the dir did not exist** — so a directory somebody else made, or one
  that merely looks like a work dir, has no marker and is reported `skip … (no marker)` forever;
- the name must be exactly `kelir-verify-<plain job>` (no `.`, no `..`, no `/`), the entry must be a
  real directory and not a symlink, and its physical path must sit **directly** under `$HOME`;
- the contents must be exactly what a work dir holds (`.kelir-workdir`, `src-tree`, `k.b64`,
  `run.sh`, `lock`, `logs`): a marker plus a stray `README` is a skip, so a user project cannot be
  killed by a name collision even if a marker is copied into it;
- a dir naming a live pid in `lock`, or written to in the last hour, is skipped; nothing is removed
  without `--yes` (the default is a dry run); `--job` and `--keep` are validated on both sides.

The 35 pre-marker dirs already on the box are exactly the case this protects: they are all reported
`skip … (no marker)`, and they are left to age out with the person who owns them.

`--keep` is now **per job** (S8): the newest N of each job, so an idle job keeps its warm `target/`
no matter how many other jobs ran after it.

### B2 — the gate lock could be held by two gates

The reviewed lock created the dir, then forked `date`, then wrote `owner`: a waiter that had already
waited 10 s saw an ownerless lock and took it. The lock is now a **record file**, published with
`ln -T` (create-or-fail): one writer wins and there is never a lock without an owner, so that window
does not exist. The record is `token`, `pid`, `start`, `cmd`.

- The command runs in its own process group (`set -m`) and publishes `$lock.grp.<token>` **before**
  `exec`, so the group is known before the wrapper becomes the command. `kill -9` of the wrapper
  therefore leaves a group that is still alive, and the next gate waits instead of starting on top
  of an orphan (S3).
- Takeover requires the pid to be gone, its `start` time to match (pid reuse, n1) and the group to
  be empty; after moving the stale record aside with an atomic `mv -T` it checks **again** before
  installing its own, which closes the TOCTOU (S4a). Release only removes a lock whose token is
  ours (S4b).
- A foreign lock — the outside driver's plain `mkdir`, no token (S2) — is never taken over: it is
  waited on and the gate exits 75 with the holder spelled out. An old-format directory lock (dir
  with an `owner` file) is still understood, and taken over only when its pid is gone.

## The should-fix items

| # | fix |
|---|---|
| S1 | `release_is_pinned_by_value_not_by_substring` parses `Cargo.toml` as TOML and compares the values. |
| S2 | see B2: a lock without a token is foreign, waited on, never removed. |
| S3 | see B2: the process group outlives the wrapper. |
| S4 | see B2: re-check after the atomic `mv -T`; release checks its own token. |
| S5 | `.config/nextest.toml` sets `leak-timeout = { period = "100ms", result = "fail" }`, and `gate.sh` scans the log for `LEAK` and names the test in `LEAKED PROCESSES`, which fails the gate. |
| S6 | far-side logs go to `$wd/logs/<stamp>`, one directory per run. |
| S7 | `AGENTS.md` describes `gate.sh --linux=<machine>`, which names the work dir `kelir-verify-<job>`; `remote-check.sh` warns when the tree is dirty (only committed HEAD travels). |
| S8 | see B1: keeping is per job. |
| S9 | no machine name in the repo: `--linux` requires `--linux=<machine>` or `KELIR_LINUX` and exits 2 with instructions otherwise; `the_linux_half_has_no_built_in_machine_name` fails if one comes back. |
| S10 | `docs/BUILD.md` headline numbers match its own table, and a nextest install section; the gate's "nextest missing" message names the install command. |
| S11 | `gate.sh` prints `FAILED TESTS:` with the names, and counts a flake once per test instead of per printed line. |

n1 is covered by the `start` time in B2; n2 by "a symlink is never a candidate" in B1; n3 by the
work-dir name validation in `remote-check.sh` and `gate.sh`.

## The bug this fix found in itself

The far-side prune body is shipped as `far_prune='<body>'`. The first version joined records with
`$'\n'` — a single quote **inside** a single-quoted string. That closed the assignment early, the
separator silently became a literal `n`, and all 35 work dirs came back as one merged record, each
then reported as "not a directory". Nothing failed loudly: the unit proofs passed, because they
extracted the body from the file (bypassing the quoting) and ran it from a file.

Fixed by keeping records in a bash array (`recs+=`) and feeding `sort -rn` one line each, and pinned
by `the_prune_body_survives_its_own_quoting`: the body must contain no single quote at all, and a
scratch home with six work dirs must list six lines and never say "not a directory". Re-introducing
the `$'\n'` fails that test.

Lesson for the next script that ships code inside a quoted heredoc/string: exercise the **transport**
(the exact `bash -lc "$body"` path), not the extracted text, and assert a count, not just a substring.

## Verified

- `cargo test --test buildinfra_review`: 17/17. `cargo test --test buildinfra`: 15/15.
- `prune` against the real home, dry run: 35 records, 0 removed, every legacy dir `skip (no marker)`.
- Prune decoys (user project, `kelir-verifyXYZ`, symlink, marker-plus-foreign-file, nested) survive on
  a scratch home on the dev box and on the Linux box; per-job keep holds there too.
- Five concurrent gates serialize strictly; a `kill -9`ed wrapper does not hand its lock on; a live
  holder is never taken over; an outside `mkdir` lock exits 75.
- Full gate, Windows and Linux: see `docs/PROGRESS.md`.
