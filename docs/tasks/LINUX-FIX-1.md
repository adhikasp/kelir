# Linux fix round 1

Running `scripts/remote-check.sh <machine>` on a Linux box shows (HEAD 76de666):
- `cargo clippy -D warnings` fails in `src/ipc.rs`: unused variable `id` (line ~132, the Unix
  `name()` branch) and dead constant `PIPE_PREFIX` (line ~30; Windows-only use). Fix with proper
  `cfg` gating, not blanket `allow`.
- unit test `ipc::tests::a_round_trip_carries_the_request_to_the_session` panics at `src/ipc.rs:348`
  with `No such file or directory (os error 2)` on Linux.
Find the root cause (likely socket path creation/bind ordering or path-length handling for Unix
sockets: sun_path is ~108 bytes and state dirs can be long; consider a short fallback location when
the session dir path is too long, recorded in the session dir) and fix it. Then run
`scripts/remote-check.sh <machine>` until fmt, clippy and **all** test suites are green on Linux, and
fix any further Linux-only failure it reveals (report what they were). Gates on Windows stay green.
Commit `Linux: fixes` (stage explicit paths), `hc send t1x "<result>"`.
