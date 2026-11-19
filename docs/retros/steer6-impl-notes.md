# STEER-6 implementation notes — the retro's loop changes

Brief: `docs/tasks/STEER-6.md`, evidence: `docs/retros/m5a.md` (+ `docs/retros/steer4.md`).
Six items, all landed: F2 (long commands), F1 (design thrash: prompt + effort knob),
F3 (multi-hunk `edit`), F4 (batching), F5 (earlier compaction), F6/F7 (prompt lines).
F1's replay-the-reasoning experiment was **not** done: the brief says do not make it a default.

## What I did, in the brief's priority order

- **F2 — the 8,191-char cut (highest value).** New `src/shellcmd.rs`: a command over
  `SHELL_CMD_MAX = 6000` bytes is written to `<call dir>/cmd.sh` (`cmd.bat` for `cmd.exe`, CRLF,
  LF never in a `cmd.sh`) and the shell runs the file with no `-c`. The threshold is
  cross-platform on purpose: the file path is exercised by the whole suite on Linux and Windows,
  so the one platform where the cut happens is not the only one that tests the fix. Two spawn
  sites needed it, not one: `bg-shim` passes `--cmd <text>` on its *own* argv, so a 40 KB
  heredoc failed with `os error 206` before the shim existed — it travels as `--cmd-file <path>`
  now. `kelir bg run` takes the same route for a 6–30 KB command; past ~32 KB the CLI's own argv
  is the ceiling (DEVIATIONS). Belt and braces: `spill::truncation_notice` appends a one-line
  notice to the result when the output contains `delimited by end-of-file` / `unexpected EOF`,
  and both tool descriptions say big files come from `edit`.
- **F1 — design thrash.** Prompt line (plan once, do not redesign; your reasoning is dropped,
  your messages are not). `reasoning_effort` per provider in `config.toml` (`low`/`medium`/
  `high`, or `none` for no field), default `medium`, sent in the body and recorded in
  `session_started.reasoning_effort`. Verified live on the OpenCode Go gateway — see
  "Measurements" below.
- **F3 — `edits`.** `edit` takes `edits: [{old, new, replace_all?}, …]` as an alternative to
  `old`/`new`: in order against the evolving text, every hunk checked before anything is
  written, one write, one result line per hunk. A miss leaves the file byte-identical (a
  unit test asserts that). The schema was trimmed after the benchmark showed the first cut cost
  929 bytes on every request.
- **F4 — batching.** `parallel_tool_calls: true` for an OpenRouter model whose
  `supported_parameters` advertises it (`models.rs` parses it now); the loop already ran a batch
  concurrently. Prompt line asks for one call set.
- **F5 — earlier compaction.** `compact_at_tokens` (default 150,000; `0` disables) caps the
  window fraction: `min(window * auto_compact_at, compact_at_tokens)`. A 1M window at 80 % was
  800K tokens, and M5-A passed 150K with 76 % of the context being old file reads.
- **F6/F7 — prompt.** Verify before claiming (name the proving command), search hygiene
  (`git grep`/`sed -n`, never `cat` a big file, skip `target/`), check each exit code, never
  chain `patch && cargo test`, calls start in the cwd and share no shell state. The base prompt
  is **1014 bytes** (budget 1024); the instruction-file paragraph was shortened to make room and
  the skills pull rule was kept.

## Measurements (item 7)

**Prompt and schema, mock only** (`python scripts/live/steer6_bench.py --old <base binary>
--new <HEAD binary>`, five scripted requests):

| | base `ed7b74e` | STEER-6 | delta |
|---|---|---|---|
| system message | 1165 B | 1475 B | +310 |
| tool schemas | 1557 B | 2149 B | +592 |
| request bodies, 5 requests | 17375 B | 22172 B | +4797 |

Both blocks are frozen into the prefix, so they are paid once per session (and cached at
99.4 %); the messages array is what is re-sent. The schema cost was 929 bytes with the first
`edits` draft and is 592 after trimming the inner descriptions that restated the outer ones.

**`reasoning_effort` on the OpenCode Go gateway** (live, 18 requests, one reasoning-heavy graph
prompt, `max_tokens` 8000, `completion_tokens` as the metric): `low` 836/1204/814/893/1452/781,
`high` 1316/938/934/991/865, no field 1166/1064/726/1236/947/1045. The spread inside one arm is
larger than between arms, so on *this* model and prompt the field is **honoured but weak**:
`low` and `high` both sit inside the no-field range. The flat field does reach the gateway
(it is accepted, not a 400, and a longer `max_tokens` budget with `high` did spend more). The
OpenRouter `{"reasoning": {"effort": …}}` object is also accepted there but bought extra
latency and no reason-token effect, so kelir sends the flat field. Also observed: on Go the
reasoning budget can consume the whole answer budget (one probe came back with empty `content`
and `finish_reason: length`), which is why `none` exists and why the knob is documented as a
request, not a guarantee. **This is the item the brief asked to verify live; it is verified, and
the honest summary is "sent, honoured weakly, not a lever that will remove the thrash by
itself".** The prompt line is the cheap half and it is in.

**The flakes.** Windows `cargo test` in one pass at 4 parallel suites has ~1 failure per run
(`review_m3::r8`, `review_m4::r18`, `review_m5a` source-watch, `e2e_verbs` stop tests), always a
*live session + `ctl`* timing test, always green on re-run and in isolation, and the baseline
commit `ed7b74e` shows the same (`e2e_verbs::a_bad_verb_never_reaches_the_session`). Serial
(`cargo test -- --test-threads=1`) is fully green. This is the recorded STEER-9 item, not a
regression from this branch.

## Gates

- Windows: `cargo fmt --check` clean, `cargo clippy --all-targets -- -D warnings` clean,
  `cargo test` green serially (310 unit, 10 e2e_steer6, all other suites as before).
- Linux (`scripts/remote-check.sh the maintainer kelir-verify-steer6`): fmt clean, clippy clean,
  315 unit + every suite green.
- No leaked processes: the benchmark and the `bg` e2e test kill by pid; `tasklist` checked for
  `kelir.exe`/`bash.exe` naming this worktree — none of mine.

## What was hard

- **The cut bit me, twice, while implementing the fix for it.** My own `cat > file <<'EOF'`
  heredocs for a >8 KB source patch were truncated (8,120 bytes with the closing marker gone),
  and I only noticed because the file was short — exactly the silent failure the retro
  describes. The `edit` tool with an empty `old` is the fix, and I used it for the rest of the
  branch. That is the strongest evidence for F2 and F3 on this box: the failure mode is not the
  dogfood model's, it is the harness's own shell.
- **Two spawn sites, not one.** The retro pointed at `bg.rs`'s `bash -c <cmd>`. That fixed the
  *shell's* argv; the 40 KB e2e still failed with `os error 206`, because the shim received the
  same text on its own argv. Fixing one and testing the other is the whole reason the e2e test
  is 40 KB and not 9 KB.
- **`should_auto`'s signature.** Adding the soft target meant a fourth argument, and a
  reviewer-written test (`tests/review_steer1.rs`) pins the window-only arithmetic. Passing `0`
  for "no target" keeps that test describing the same thing without weakening it.
- **A `kelir bg run` ceiling I could not remove.** The CLI takes the command as an argument, so a
  command past the process command-line limit cannot be *given* to it. Documented rather than
  faked with a stdin flag nobody asked for.

## Where the tools or the prompt slowed me down

- **`bash` heredocs over 8 KB are unusable on this box** and nothing in the tool's own text said
  so before this branch (now it does, in the description and in the notice). Every large patch
  cost a retry until I switched to `edit` with an empty `old`.
- **No `rg` on the box**, and `grep -r .` at the repo root walks `target/` (the F7 evidence).
  The prompt now says so; the harness still ships without `rg`.
- **The flaky live-session suite** made "is my change responsible?" a question I had to answer
  with a baseline worktree and repeated runs instead of one green run. That is ~10 minutes per
  full pass in wasted re-runs and it is the STEER-9 item.

## Files

- new: `src/shellcmd.rs`, `tests/e2e_steer6.rs` (10 tests), `scripts/live/steer6_bench.py`,
  `scripts/live/steer6_bench.sh`
- config: `src/config.rs` (`reasoning_effort`, `compact_at_tokens`, `DEFAULT_REASONING_EFFORT`,
  `REASONING_EFFORTS`), `src/prompt.md` (1014 B)
- provider/loop: `src/provider.rs` (`reasoning_effort`, `parallel_tool_calls`, `build_with`),
  `src/models.rs` (`parallel_calls`), `src/compact.rs` (`DEFAULT_SOFT_TARGET`,
  `auto_threshold`), `src/session.rs` (records `reasoning_effort`, passes the parallel flag)
- tools: `src/tools/edit.rs` (`edits`), `src/tools/mod.rs` (three descriptions),
  `src/tools/bash.rs` (the file route), `src/spill.rs` (the truncation notice)
- job path: `src/bg.rs` (`cmd_file` on `Launch`, `Status::cmd_path`, the shim's file route),
  `src/cli.rs` (`--cmd-file`)
- docs: `docs/DEVIATIONS.md` (six entries for this job)
