# BUILD-INFRA — faster, calmer builds and gates (no product behaviour change)

Measured 2026-11-19 with three workers live: Linux cold test build 62 s, cold clippy 29 s, test execution 96 s (20 test
binaries, sequential), release with fat LTO 75 s, incremental after touching lib.rs 17 s; Windows incremental test build
27-36 s, test execution ~137 s, clippy 242 s observed; 324 crates, 20 test binaries, a per-worktree target dir of 5-15 GB
(1.75 GB of PDBs), no `.cargo/config.toml`. Load flakes cost more than the compiles: they forced 2-4x re-runs; the suites
are green when run serially. One build-infra worktree at a time; lands before M5-B.

Land in this order, measuring before/after with the same commands (cold and warm), table in `docs/BUILD.md` and the retro.

1. **Gate mechanics (no repo change)**: exactly one gate at a time across all jobs (the orchestrator's gate script now holds
   a lock; document it for workers), `CARGO_BUILD_JOBS=4`, at most 4 test threads per worker.
2. **Test runner**: `cargo-nextest` for gates (`cargo nextest run --no-fail-fast --retries 2 -j 4`); keep `cargo test --lib`
   for the fast loop. Install it (Windows and the Linux box). Nextest runs each test in its own process, so re-run the
   no-leaked-process and no-visible-window checks afterwards (AGENTS.md rules; scripts/live flash matrix). Coordinate with
   STEER-9: event-driven waits remain the right fix for timing assertions, not bigger ceilings, and `--retries` must not hide
   a real failure (report which tests needed a retry).
3. **Linux gate** (`scripts/remote-check.sh`): stop deleting the tree each run. Reuse one work dir per job (`git fetch` +
   `git checkout -f`, or a persistent `CARGO_TARGET_DIR`) so the 62 s build + 29 s clippy are not paid every run. Add a prune
   subcommand: keep the last 2-3 work dirs per active job (the Linux box has ~17 stale `~/kelir-verify*` dirs, 57 GB).
4. **Defender exclusions** are already applied by the maintainer for the repo, worktrees, scratch, `.cargo`, `.rustup`
   (real-time stays on). Verify with `(Get-MpPreference).ExclusionPath`, do NOT widen them; record in the notes.
5. **`.cargo/config.toml`**: `[profile.dev]`/`[profile.test]` `debug = "line-tables-only"` and, for
   `x86_64-pc-windows-msvc`, `linker = "rust-lld"` (ships with the toolchain). Verify a clean build and the full suite on
   Windows BEFORE adopting; adopt only if the times drop and everything stays green; otherwise revert and record why. Linux keeps
   its default linker.
6. **`[profile.review]`** inheriting release with `lto = "thin"`, `codegen-units = 16` for reviewer binaries; fat LTO +
   `codegen-units = 1` stays for anything shipped.
7. Only if cheap: sccache for cross-worktree dependency caching; move the shared `tests/support` module into a dev-dependency
   helper crate.

Rules: no product behaviour change, no weakened tests, worktree workflow, explicit-path commits; gates green on Windows AND
Linux or it does not land. If nextest or rust-lld complicates the gate more than it saves, keep the measured win, drop the
rest and say why. Report the final HEAD and before/after numbers.
