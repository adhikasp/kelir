# CI fix review — first public CI failure (run 44881658482)

Base `job/ci-fix` @ c69ae48. CI: Windows clippy on rust 1.100 failed; macOS `cargo test`
had 4 failures (763 passed); Linux and macOS clippy were clean. No production
behaviour was changed — the fixes are a clippy-compatible rewrite and three
platform-correct tests. The local toolchain (1.99.1) reproduces none of these
directly, so each fix is derived from the CI log and the platform contracts.

## 1. Windows clippy: `clippy::needless_bool` in `src/credentials.rs:597`

Root cause: clippy 1.100 sees `if !metadata.is_file() { return false; }` followed
by cfg blocks whose Windows branch is `true` and suggests `metadata.is_file()`.
Applying that suggestion literally would delete the Unix executable-bit check
(the suggestion only makes sense on the non-Unix branch).

Fix: keep the early return (fast path, identical behaviour) and move the
non-Unix literal into a `#[cfg(not(unix))] fn is_executable_non_unix` helper.
Clippy cannot fold the guard into a helper *call*, so `-D warnings` passes on
1.99 and 1.100; the Unix `0o111` check and the Windows `true` are unchanged.
Verified with `cargo clippy --all-targets -- -D warnings` locally.

## 2/3. macOS tests: start-time identity (`src/bg.rs:2586`, `:2631`)

Root cause: `crate::proc::start_time` is Linux/Windows only (`src/proc.rs:319`);
on macOS it returns `None`. `start_time_text(me)` is therefore `None`, so the
synthetic status has no start time and `identity()` returns `NoPlatform`, not
`Same`. The tests asserted `Same` unconditionally.

Fix: derive the expected first answer from `start_time_text(me).is_some()`
(`Same` where start times exist, `NoPlatform` on macOS) and assert that. The
NoRecord and Different checks remain: the command-pid test's wrong-start-time
case still expects `Different` where the platform can compare and `NoPlatform`
where it cannot; `None` and pid 0 still expect `NoRecord`. Windows and Linux
are unchanged (`start_time` is `Some`), so their `Same`/`Different` assertions
are as strict as before.

## 4. macOS test: gate descendant survived (`src/gates.rs:1971`)

Root cause: the gate is killed at 600 ms; the script asks a detached child to
write `flag.txt` after `sleep 3`, then the test waits a fixed 4 s. The child's
sleep starts when the shell schedules it, not at gate start; on a busy macOS
runner (this CI job runs alongside two other OSes) the descendant can be
scheduled late and write the marker just after the fixed check already passed.

Fix: measure a 5 s deadline from the end of the gate instead of a fixed 4 s
sleep (the script's own delays are 1 s + 3 s). No cfg: the same (stricter)
no-leak guarantee applies on all three OSes; the production tree-kill and
reap paths were not touched. The hidden `gate-guard` still covers a killed
session; the token sweep's documented Linux-only `/proc` limitation is
unchanged (`docs/reference/gates.md`).

## 5. macOS test: "bind creates the socket file" (`src/ipc.rs:851`)

Root cause: BSD/macOS `sockaddr_un.sun_path` is 104 bytes (Linux 108), so
`MAX_SOCKET_PATH` is 103 there. The test builds a dir named
`kelir-ipc-client-unlink-<ULID>` under `std::env::temp_dir()`; on macOS CI the
temp prefix plus that name plus `/sock` exceeds 103 bytes, so `bind()` takes
the production fallback path (short path in the temp dir, recorded in
`sock.path`). The test then looked for the canonical `<dir>/sock`, which is
correctly not created in that case.

Fix: pick a short directory name when the canonical path does not `fits()`;
otherwise keep the existing directory. Production path resolution (the
fallback and its record) is untouched and is covered by the neighbouring
`a_too_long_session_dir_binds_a_short_recorded_socket` test. No path contract
changed.

---

# Second public CI fix (run 44885707434) — branch job/ci-fix2

Base `job/ci-fix2` @ d6bc110. Eight remaining failures: 5 Windows (embedded
text checked out as CRLF), 2 macOS (start-time identity `NoRecord` branches),
1 macOS (a gate descendant survived). No production prompt/reference bytes
change; the fixes are a checkout-level EOL rule, two platform-aware test
expectations and a portable macOS tree-kill.

## A. Windows 5 failures: CRLF checkout of embedded text

`src/prompt.md` is 1014 bytes with 20 LF newlines. On a Windows CI runner
Git converts those to CRLF, making it 1034 bytes; the byte-exact prompt
snapshot and the 1024-byte budgets then fail, and the generated reference
blocks (config/cli/env/verbs/events) go stale because the `include_str!`-ed
`.rs` sources are CRLF too. Local Linux (LF checkout) passes all five.

Fix: a root `.gitattributes` forcing LF for every embedded text family:
`*.md`, `*.snapshot` and `*.rs`, each `text eol=lf`. That covers the prompt,
its snapshot, all `docs/reference/*.md`, the builtin skills, and the
`include_str!`-ed source files (`config.rs`, `event.rs`, `verbs.rs`) read by
the docs drift test. Other file types keep Git's default handling.
Verified: `cargo test --lib docs::tests` (24 passed) and
`cargo test --lib sources::tests` (25 passed) locally.

## B. macOS 2 failures: start-time identity `NoRecord` branches

`identity()` at `src/bg.rs:306` returns `NoPlatform` before it ever looks at
the recorded start time when the platform cannot report one (macOS:
`proc::start_time` is `None`). The first ci-fix made the `Same` cases
platform-aware but left two `NoRecord` asserts unconditional:
`a_status_is_matched_with_its_own_process_not_just_its_pid` (missing
`shim_started`) and `a_command_pid_is_only_trusted_with_its_own_start_time`
(missing `pid_started`). On macOS they saw `NoPlatform` and failed with
`left: NoPlatform, right: NoRecord`.

Fix: both asserts now expect `NoRecord` where `start_time` is available
(Windows/Linux) and `NoPlatform` on macOS, mirroring the `Same` fix. The
`Different` case, the pid-0 `NoRecord` case and every liveness assertion
are unchanged. Windows and Linux keep their strict `NoRecord` checks.

## C. macOS 1 failure: gate descendant survived

`reap::kill_run` was a no-op off Linux. On macOS only the process group was
signalled; a gate script's detached `(sleep 3; echo alive > flag.txt) &`
child could survive and write the marker.

Fix: `kill_run` now runs on macOS too. It reuses the bounded Linux
set-based descendant walk (5 rounds, 15 ms) with a macOS `process_table()`
built from `ps -ax -o pid=,ppid=`, and a token sweep built from one
`ps axww -e -o pid=,command=` invocation. The token sweep matches the exact
`KELIR_GATE_RUN=<token>` field; `ps` output is never printed or logged, and
only exact PIDs are signalled (never this process, never a group). A pure
parser unit test covers whole-field matching. The documented Linux-only
limitation in `docs/reference/gates.md` was updated. Windows/Job Object
behaviour and the Linux `/proc` path are unchanged.

## Gate evidence

Full gate `bash scripts/gate.sh --linux=the maintainer` at HEAD `17da1ae`, exit 0,
`gate: PASS`. Logs: `/c/Users/the maintainer/kelir-scratch/gate-job-ci-fix2-20261121T085925Z/`.

- Windows: fmt ok, clippy ok, nextest 1433 passed / 6 skipped (230.5 s),
  doctest ok.
- Linux (`the maintainer`): fmt ok, clippy ok, nextest 1435 passed / 3 skipped
  (123.8 s), doctest ok.
- Targeted: `cargo test --lib docs::tests` 24 passed;
  `cargo test --lib sources::tests` 25 passed;
  `cargo test --lib bg` identity tests 2 passed;
  `cargo test --lib gates::tests` 28 passed (Linux);
  `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` clean.

The macOS-only tree-kill path was compile-checked by temporarily swapping the
target cfg so the macOS functions were compiled on this Windows host (then
restored); the Linux gate compiles the Linux path. A pure parser unit test for
the macOS `ps e` sweep is included in `src/gates.rs`.
