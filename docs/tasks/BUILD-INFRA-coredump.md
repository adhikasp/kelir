# BUILD-INFRA: the Linux gate must not leak core dumps

Observed 2026-11-20 on the Linux gate machine: the system core-dump store grew to ~900 MB / 2,660 files, ~290 KB each, from
`.../target/debug/kelir` SIGABRT: the deliberate `KELIR_CRASH_AT` crash-injection tests (M3) that the gate runs abort the process on
purpose. A core file from an intentional abort is pure waste.

## Build
- In `scripts/remote-check.sh` (the far-side command that runs fmt, clippy, nextest, doctests): run everything under `ulimit -c 0`
  (set it before any test binary starts; it is inherited by the children). Do it inside the gate, not as a host setting, and do not use
  sudo or touch system config.
- Mirror it for any other place the repo's scripts run the crash tests on a Unix box (`scripts/gate.sh` local Linux/macOS runs: a
  `ulimit -c 0` in the Unix branch).
- Document under "rules" in docs/BUILD.md that the `ulimit -c 0` is deliberate (why, the observed numbers) so a future editor does not
  drop it. Add to AGENTS.md's Linux gate paragraph one line.
- Add a test if the script contract can carry one (e.g. the generated far-side command text contains `ulimit -c 0` before the test
  stage; or, on a Unix host, a test that runs a child under the gate's wrapper and asserts `ulimit -c` prints 0).
- Verify on the Linux box: run the crash tests (e2e_m3) through the gate and show that the dump count in the core-dump store does not
  grow (`coredumpctl list | wc -l` or the file count before/after; read-only, no sudo; if it needs root to read, say so and instead check
  `ulimit -c` inside the remote run).
Gate: `bash scripts/gate.sh --linux=<machine from your prompt>`. Commit `buildinfra2: ...` (explicit paths), note in docs/retros as
buildinfra2-impl-notes.md and in the BUILD-INFRA retro notes, final answer "buildinfra2 ready: <commit>".
