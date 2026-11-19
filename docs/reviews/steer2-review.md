# steer2 review (reviewer/tester, fresh)

Diff `e13662a..57a7790` (job/steer2). **Verdict: PASS WITH FIXES** — 0 blockers, 3 should-fix, 3 nits.

## Gates
- `cargo fmt --check` OK; `cargo clippy --all-targets -- -D warnings` OK.
- `cargo test --no-fail-fast` (Windows): 680 passed, 2 failed — both are my new proving tests (below). Every pre-existing suite green.
- Linux: `bash scripts/remote-check.sh the maintainer kelir-verify-steer2-rev` rc=0: fmt, clippy, nextest 669 passed / 9 skipped, doctest OK. (My `tests/review_steer2.rs` was not in that export; not run on Linux.)
- Release binary 6.7 MB.

## Findings
| # | sev | where | finding |
|---|---|---|---|
| 1 | should-fix | src/credentials.rs:235,263 (`read_store` → `parse_auth_export` :322) | A store entry already expired on the **first** read is an immediate usage error; the brief says re-read once (opencode refreshes itself when it runs) and only then fail. Test `expired_on_first_read_is_reread_once_and_recovers` (first read expired, second fresh) fails with exit 2 "credential expired…". The near-expiry (<2 min) re-read works (`near_expiry_prefers_the_fresher_second_read` passes). Fix: in `store_credential`, treat an expired parse as "re-read once", then fail. |
| 2 | should-fix | src/credentials.rs:188-198 vs docs/SPEC.md:338 | OpenRouter: SPEC/brief say `OPENROUTER_API_KEY`, **then** `key_env`. Code makes `key_env` *replace* the default name. `[providers.openrouter] key_env="UNSET_VAR"` with `OPENROUTER_API_KEY` set → "missing API key" (test `openrouter_key_env_is_a_fallback_not_a_replacement` fails). Matters for the migrated maintainer config (`key_env = "KELIR_OPENROUTER_API_KEY"`). Either fix code or correct SPEC and DEVIATIONS. |
| 3 | should-fix | src/config.rs `load` (providers-table check) | Migration message for the maintainer's real config (`[providers.kelir]`) is the generic ``unknown provider `kelir`: kelir has exactly two…``. It does not say it came from a `[providers.kelir]` table, which file, or "rename the table to `openrouter` (key_env still works)". It also makes `kelir doctor`, the diagnostic tool, exit 2 on that config. Clear and not a crash, but not a migration message. |
| 4 | nit | src/config.rs warning strings | `\` line continuation eats the space: "...the environment(OPENROUTER_API_KEY / ..." and "`key_env`,`key_cmd`". |
| 5 | nit | src/doctor.rs | `kelir doctor` exits 1 and prints FAIL when the *non-selected* provider has no key (OpenRouter FAIL with only Go configured). |
| 6 | nit | src/credentials.rs:397 | helper stdout is read unbounded into memory for up to 5 s (30 MB export handled fine; a pathological helper could grow far larger). |

## Secret handling (focus) — no leak found
Real token and refresh token obtained only inside my own python process via `opencode auth export opencode`; compared as raw bytes (UTF-8 and UTF-16LE), never printed or written.
- After real live runs (`run`, `ask`, `doctor`) on `opencode-go`: scanned the state dir (events.jsonl, commands/, models cache), cwd, stdout/stderr files, `%APPDATA%\kelir`, `CrashDumps`, repo `docs/ src/ tests/`, and every process command line (Get-CimInstance Win32_Process): **0 hits** for access or refresh token. `session_started.key_origin` = `opencode auth export opencode`. Same scan for the OpenRouter key after the OpenRouter live run: 0 hits.
- Mock tests: a provider 401 body echoing the Bearer token (store key and env override) does not reach events/stdout/stderr (redact in provider.rs:274). Garbage/empty/inactive/other-integration/no-access exports: clear exit-2 messages, no token, no panic.
- TUI scrollback pty replay: not separately driven; the TUI renders from events/log which were scanned clean.

## Break-it results (tests/review_steer2.rs, Windows `.cmd` shim)
hang (30 s helper): stopped < 12 s, "did not answer", no request, no leftover `ping`; 30 MB export OK; two active entries → first active; inactive / other integration / garbage / empty → exit 2 clear; junk `KELIR_GO_API_KEY` → override wins, store not run, exactly 1 request, no retry, error event; literal `key` (top-level and in provider table) → warning, never sent, falls to "missing key" when env absent; provider-name typos (`opencode_go`, `OpenRouter`, empty, ` openrouter`) → exit 2 naming both providers (no "did you mean" suggestion); `doctor` with expired store → exit 1, says expired, no secret.
Missing opencode: covered by implementer's e2e (`not found on PATH`, names override).
Window/console: from a CREATE_NO_WINDOW parent, 4 `kelir doctor --provider opencode-go` runs through the real `opencode.cmd` showed **no new visible top-level window** (EnumWindows sampling). No leaked kelir/opencode/ping processes from my runs (kelir-s4.exe seen belong to other worktrees; untouched).

## Live drive (Herdr panes, `hc run --bg`)
- `live-steer2-go2`: config `provider="opencode-go" model="deepseek-v4.1-flash"`, no `KELIR_GO_API_KEY`. `kelir doctor`: `opencode-go … key: opencode auth export opencode · expires 2026-11-24T09:51:54.282Z · read in 1788 ms`. `kelir run --json`: exit 0, `final_text:"ok"`, 1 step, 1179 prompt tokens. `kelir ask`: "Hi there." exit 0. Real Go endpoint verified.
- `live-steer2-or`: OpenRouter default model, exit 0 `final_text:"ok"`; doctor `env OPENROUTER_API_KEY`; literal `key` config prints the warning. (live.env also exports KELIR_GO_API_KEY, so Go showed `env KELIR_GO_API_KEY` there — override wins as designed.)
- Maintainer's real `%APPDATA%\kelir\config.toml` (`[providers.kelir] kind="openrouter"`): `kelir doctor/run/ask` all exit 2 with ``unknown provider `kelir`: kelir has exactly two…`` — no crash, not silently accepted (see finding 3). That config has no literal key (only `key_env`).
- Latency: `opencode auth export` 225 ms warm, 1788 ms in the first pane read (cold); consistent with implementer's numbers.

## Not covered
Linux run of my extra tests; pty-replay of the TUI; opencode absent on a real Windows PATH (mock only).
