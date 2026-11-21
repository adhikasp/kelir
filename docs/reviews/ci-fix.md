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
