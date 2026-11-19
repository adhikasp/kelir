# STEER-2 implementation notes

Brief: `docs/tasks/STEER-2.md` — exactly two providers, and OpenCode Go's credential read from
opencode's own store through `opencode auth export opencode`, plus a `kelir doctor`.

## What landed

| commit | what |
|---|---|
| `dfb9ebb` | two providers only (`ProviderKind` loses `Custom`; the name *is* the provider), `key_cmd`/`key_env`/`key_origin`/`key_expires_at_ms` on `ProviderConfig`, a literal `key` warned about and never used, `config::load` still pure; new `src/credentials.rs` (precedence, the bounded silent helper run, the auth-export parse, the two-minute margin, redacting `Debug`) and `src/doctor.rs` (`kelir doctor [--json] [--provider]`) |
| `8638aac` | the session resolves the key at start, keeps it in `active_provider`, records `session_started.key_origin`, re-reads a key within two minutes of `expires` before a step, re-reads once on 401/403 and retries the request once, and resolves the *other* provider's key on a `model` swap |
| `f4201b0` | `tests/e2e_steer2.rs` (8 tests, a fake `opencode` on the child's `PATH`) and the provider-swap verb test moved off the removed custom provider |
| `948cfbd` | `docs/SPEC.md` **Providers** (the whole policy), the `session_started` row, a mention in the CLI line, and the `docs/DEVIATIONS.md` section |
| `0d5d0cc` | the file-name test made portable (a backslash is an ordinary character on Unix) |

Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` — Windows
394 lib + 22 test binaries, all green; Linux (`scripts/remote-check.sh the maintainer kelir-verify-steer2`)
394 lib + the same suites, fmt and clippy clean. Release binary 6.7 MB.

Live check on the dev box (not the reviewer's drive, just a sanity run with the real credential):
`kelir doctor` reports `opencode auth export opencode · expires 2026-11-24T09:51:54.282Z · read in
1221 ms`; `kelir run --provider opencode-go --model deepseek-v4.1-flash` answered `ok` in one step,
and grepping the session dir for the store's `value.access` found nothing.

## Numbers

- `opencode auth export opencode`: 1221 ms cold (opencode's service had to start), 289/308/296 ms
  warm.
- `kelir ask` on `opencode-go`, same prompt: 1964/2063 ms through the store vs 1661/1483 ms with
  `KELIR_GO_API_KEY` set — the read is ~0.4 s of a start dominated by the first network round trip.

## What was hard

- **Precedence, written down once.** The brief says "explicit env/`key_env`/`key_cmd` override >
  opencode store" for Go and env → `key_env` → `key_cmd` for OpenRouter. Both are now one list in
  `resolve_with`, with the environment and the command runner injected, so the order is tested
  without touching this process's environment or spawning anything (10 of the 16 credential unit
  tests).
- **Not leaking the key.** The key is stored in exactly one place (`active_provider.key`, and the
  `Client` built from a clone), `Credential`'s `Debug` prints `[redacted]`, and every message names
  the *source*. `key_cmd` is named by its file name only — the first version put
  `C:/Users/…/bin/getkey.cmd` into `session_started.key_origin` and `doctor`, which the public-log
  rule forbids. The e2e tests grep every file a run wrote for the token.
- **Re-reading without inventing a loop.** The 401 path is once per request (`reread` is reset only
  when the request is rebuilt for another reason), and a re-read that returns the same value gives
  up at once. That is what keeps `tests/e2e_steer5.rs::a_4xx_refusal_is_not_retried` meaningful: with
  the test harness's env key the store is never consulted, so a 401 is still exactly one error and
  one request.
- **Windows console discipline.** The helper is started with `CREATE_NO_WINDOW |
  CREATE_NEW_PROCESS_GROUP`, no inherited stdin, and killed by pid/group on timeout — never by image
  name. A `.cmd` shim goes through `COMSPEC /C` with `raw_arg`, which is the only spelling that
  survives a path with a space in it.
- **`cmd`'s own builtins dislike forward slashes.** A `.cmd` whose body says
  `type "C:/Users/…/key.txt"` works only when the cwd is on the same drive root; from a temp cwd it
  is "The system cannot find the file specified." The test shims now write native separators
  (`%*`, `type "C:\…"`), which is a Windows-only detail that cost an hour and is worth knowing for
  every future test fixture.

## Where the tools or the prompt slowed me down

- The compacted summary I started from said the 401 re-read made
  `tests/e2e_steer5.rs::a_4xx_refusal_is_not_retried` fail. It did not (it passes unchanged): the
  harness sets `KELIR_GO_API_KEY`, so the OpenRouter run in that test never has a store to re-read.
  The real breakage was `tests/e2e_verbs.rs::model_swap_can_change_the_provider`, which used a
  `[providers.alt]` custom provider. Re-checking the stated failure before acting saved a wrong fix;
  a summary's "open" items are worth re-running rather than trusting.
- Two e2e tests in unrelated suites (`e2e_verbs::say_mid_turn_is_in_the_next_request`,
  `e2e_tui::a_ctrl_c_before_the_first_message_does_not_stop_the_turn`) failed once each under the
  parallel full-suite load and pass alone 3/3 and on every later full run. They are the timing
  flakes the STEER-5 row already records, not new failures; worth a look in a later timing pass.
- `sed -n` over the big files spilt to files rather than to the transcript, which is the right
  behaviour but made "read the diff" slower than `git diff --stat` + targeted reads.

## Left open

- `docs/PROGRESS.md` has no STEER-2 row: several jobs run in parallel and that file conflicts; the
  orchestrator records the row at merge.
- `scripts/live/` already passes `--provider openrouter` everywhere and names no custom provider, so
  nothing there needed changing. A Go scenario would need a key or a store; not added.
