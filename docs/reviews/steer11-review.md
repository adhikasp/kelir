# steer11 review — self-documentation (`docs/reference/`, `kelir docs`, drift test, builtin skill)

Reviewer: fresh QA. Range `b7b60c6..a1fa30f` (branch `job/steer11`). **Verdict: PASS WITH FIXES — 0 blockers, 6 should-fix, 7 nits.**
Every should-fix is a factual error in a doc line (the docs claim X, the code or a real run does Y) or a hole in the drift guard.

## Pyramid (run independently)
- Windows `scripts/gate.sh`: fmt ok, clippy ok, nextest **903 passed, 3 skipped**, doctest ok, `gate: PASS`.
- Linux `scripts/remote-check.sh the maintainer kelir-verify-steer11-rev`: FMT/CLIPPY/NEXTEST (**903 passed, 3 skipped**)/DOCTEST ok, rc=0.
- `cargo test --lib docs`: 18 passed. `kelir docs --check` ok (12 topics); `--generate` twice: "nothing to generate", git tree unchanged.
- No leaked processes from my runs (a `bg run` I started was killed by exact PID; `tasklist` shows no kelir.exe).

## Should-fix
| # | file:line | claim | reality (evidence) |
|---|---|---|---|
| S1 | `docs/reference/config.md` row `prompt` ("`<config>/prompt.md` and `<cwd>/.kelir/prompt.md` win over it") and section "Where the prompt…" ("It loses to `<config>/prompt.md`"); `sources-and-skills.md` "Overridden, weakest first: the config's `prompt` file, `<config>/prompt.md`, `<cwd>/.kelir/prompt.md`" | config `prompt` is weaker than machine `prompt.md` | `config.rs::load_prompt` returns the configured file first and reads `<config>/prompt.md` only when `prompt` is **unset**. Live: `--config c.toml` (prompt=kp.md) with `KELIR_CONFIG_DIR/prompt.md` present gives `session_started.sources[0]` path = `kp.md`, system text starts `CONFIGKEYPROMPT`. Only `<cwd>/.kelir/prompt.md` beats the key. |
| S2 | `files.md:38` "`kelir ls` (and `ctl latest`) can pick the newest" | a `kelir ls` command exists | `kelir ls` gives `error: unrecognized subcommand 'ls'`, exit 2 (not in `--help`). |
| S3 | `env.md` (generated row from `src/envs.rs`) "`KELIR_HOME`: the base directory `KELIR_STATE_DIR` and `KELIR_CONFIG_DIR` derive from"; `env.md` Notes; `files.md` "`KELIR_HOME` replaces the base both derive from" | KELIR_HOME relocates state/config | `paths.rs`: it only changes `home()`. On Windows `LOCALAPPDATA`/`APPDATA`, on Linux `XDG_STATE_HOME`/`XDG_CONFIG_HOME` win when set. Live: `KELIR_HOME=<scratch> kelir bg run -- echo hi` wrote to the real `%LOCALAPPDATA%\kelir\bg\` (I deleted my 3 files). A test author following the doc would touch real state. Say: KELIR_HOME only replaces `$HOME`/`USERPROFILE` (skills, `~`); use `KELIR_STATE_DIR` + `KELIR_CONFIG_DIR` to isolate. |
| S4 | `troubleshooting.md` "A background job I started is gone": "`bg ls` shows it as missing" | listed as missing | Without its `.status` file `kelir bg ls` prints `no background jobs in …`, `bg out` says `no background job`, `bg cancel` exits 2. It is simply absent. |
| S5 | guard `src/envs.rs::no_env_read_happens_outside_this_module` (env.md: "a drift test fails when a `std::env::var` call appears anywhere else") | no env read outside the registry | It greps the text `env::var(` only. Mutation: `std::env::var_os("ZZ_OS")`, `use std::env::var_os as v; v("ZZ_ALIAS")`, `std::env::vars()` in `src/util.rs` all leave `cargo test --lib envs` and `docs` **passing**. (A quoted `KELIR_*` literal is caught by the docs test; other names are not.) Today's code is clean (grep finds no such reads), so this is a hole, not a lie. Extend the grep to `env::var`/`var_os`/`vars`/`vars_os`. |
| S6 | Brief item 1: "source pointer `src/config.rs:LINE`"; docs use `src/config.rs:load` (README explains: function names don't move) | line pointers | Defensible, but a deviation from the brief that is not in `docs/DEVIATIONS.md`; and every top-level row says `load`, a ~200-line function, which points nowhere useful. Record the deviation; consider the field name instead of the function. |

## Drift-test mutation results (code mutated, `cargo test --lib docs`/`envs`, reverted with `git checkout -- src`)
| mutation | caught? |
|---|---|
| new `RawConfig` field | yes (`every_config_key…`) |
| new verb arm in `verbs::parse` | yes |
| new clap flag | yes (`the_reference_on_disk_is_what_the_generators_produce`) |
| `std::env::var("KELIR_ZZTOP")`; `.env("KELIR_ZZCHILD",…)`; computed `std::env::var(&n)` | yes |
| `std::env::var_os("ZZ_OS")`, aliased `var_os`, `env::vars()` | **no** (S5) |
| serde `rename = "model_id"` / `alias` on an existing key | **no**: docs would keep saying `model`; no such attribute exists today |
| new field on an `EventBody` variant | my mutation did not compile (struct literals), so unproven; the field lists are hand-typed and I found no field check: gap |
| default changed (`spill_limit` 10240→20480, `DEFAULT_KEEP` 4→7) | **no**: defaults are prose in the tables, so the doc silently goes stale |
| hidden clap *flag* | listed in cli.md (hidden *subcommands* are skipped); inconsistent, no such flag today |
| slash command / TUI key added | not covered by any drift check (tui.md is hand-written) |

Defaults, event fields and the TUI surface are therefore only as fresh as the author remembers: "no drift" holds for names, not for values.

## Claim verification (config/CLI/env/verbs/events)
Verified true against code (and a run where noted): config key defaults/units (`spill_limit` 10240, `window` 128000, `auto_compact_at` 0.8 with (0,1] check, `compact_at_tokens` 150000, `compact_keep` 4, retry 2000/120000/1800, `promote_after` 600 + `KELIR_PROMOTE_AFTER`, `max_steps` 0 = no cap, `reasoning_effort` values); `retry_max_attempts=2` live: two `error` events, `turn_ended{outcome:paused,reason:provider}`, exit 0, as documented; verb table vs `Verb::scope`; event field lists vs `EventBody` (all 16 match by reading); credentials constants (5 s, 2 min); instruction/skill byte limits; `kelir docs` exit codes (unknown topic 1 with "did you mean", `--find` miss 1, `--generate` with no checkout 1 with a clear message). Config merge (project wins per key) matches `merge()`.

## "How do I…" questions
Live, as an agent in a Herdr pane (`live-steer11`, deepseek-v4.1-flash-1017 via OpenRouter, cwd with no repo, told to use only `kelir docs`): it ran `kelir docs`, then `config`/`providers`/… and answered five questions with exact names: cap steps (`max_steps`/`--max-steps`), retry budget (`retry_max_wait`, `retry_max_attempts`, base/cap), extra skills dir (`skills_dirs` and search order), resume (`kelir resume <ID> [TASK]`, `--resume`), log location (`<state>/sessions/<id>/events.jsonl`). I cross-checked each against code/runs: correct. Others answered from the docs and verified by me: change model (`model`/`--model`/`ctl model`), view an image (in-session only, cli.md accurate), stop a job (`ctl stop`; `bg cancel` killed the tree). "See what the model will see": only implicit (fold of events, `session_started.system`); no print command exists, and the docs do not say so. "Rotate the credential": no section; derivable from providers.md key order, but worth two lines. `--find max steps` ranks `cli` (three duplicate flag rows) above `config`/`troubleshooting`: works, ranking mediocre (nit).

## Other checks
- Embedded equals repo: `kelir docs <topic>` is byte-identical to `docs/reference/<topic>.md` (`cmp`; the lib test asserts it too). Reference total 66,491 bytes (budget 128 KB). Release binary 7.59 MB (< 20 MB). I did not build a baseline binary (3-min LTO); the embedded text is ~66 KB. `kelir docs --list` 40 ms, `docs config` 69 ms wall, and docs are not in any other command's startup path.
- Generated markers (on a copy via `KELIR_DOCS_ROOT`): hand-written prose survives regeneration, a stale marker block is rewritten, second `--generate` is "nothing to generate", `--check` is rc 1 on stale and ok after.
- Builtin skill: appears in `session_started.sources` as name `kelir`, kind `skill`, empty path, with no repo on disk; description is fine against the brief. Machine layer, so a project `kelir` skill overrides.
- Base prompt: 1023 bytes (<= 1024), snapshot-pinned, deterministic; the line is `kelir docs`: the reference.
- Scrub: patterns in `scrub-patterns.txt`: 0 hits in `docs/reference`, `src/docs`, `src/prompt.md`; extra grep for home paths, hostnames, emails: 0.
- SPEC "Self-documentation" and AGENTS.md "Docs are part of done": present and accurate.

## Nits
N1 `config.md` `shell` default: Linux also tries `$SHELL` before `sh`; Windows falls back to `cmd.exe /C`, never `sh`.
N2 `kelir ctl --help` (pre-existing text, republished in cli.md) lists verbs "say, later, stop, model, tools, end", omitting `skills`, `compact`, `view`, `bg`.
N3 tui.md misses slash aliases (`/now`, `/say_now`, `/exit`, `/?`) and Left/Right/Home/End/Delete.
N4 Skill body ends with repo-development advice (`scripts/gate.sh`, "fresh reviewer, not you") a deployed session cannot use.
N5 No unprompted-discovery live test (does an agent reach for `kelir docs` from the prompt line alone?). One A/B would tell.
N6 `gates-and-mcp.md` absent (waits for M6/M10, per the brief); kelir reads no `HC_*` variable, so omitting them is right.
N7 Hidden clap flags would be documented (see table).

Merge recommendation: fix S1-S4 (text only) and S5 (one line in the guard), record S6 in DEVIATIONS; re-check with `cargo test --lib docs` and the gate.
