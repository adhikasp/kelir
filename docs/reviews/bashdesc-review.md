# Review: `bash` gets a human-readable `description`

Reviewed `685b6ff..830c96e` on branch `job/bashdescrev` (base `job/bashdesc`, plus my
review-only tests at `e9b7aab`). Reviewer: fresh QA agent. No product code was changed.

## Verdict: PASS WITH FIXES (0 blockers, 1 should-fix, 2 nits)

The queue item's contract holds: the optional label reaches the settled TUI line, the
live `⠹` row and the headless progress line; the command stays byte-exact in the log,
NDJSON, the output file and `/expand`; missing/wrong-type/blank labels fall back to the
old rendering; the cap is 120 **characters**, whitespace is folded, and the docs drift
test is clean. Gates are green on Windows and Linux. The one should-fix is a real
terminal-safety gap the feature introduces: the label is not stripped of control bytes
before a headless line is written into a terminal/Herdr pane, while the project already
treats untrusted labels that way in `src/herdr_ctx.rs:155-166`.

## Gates and proving commands

Run on branch HEAD `e9b7aab` (my review tests included), full gate:

```
bash scripts/gate.sh --linux=the maintainer
  fmt: ok (2081 ms)
  clippy: ok (44432 ms)
  nextest: ok (261977 ms)
  result: Summary [ 202.478s] 1367 tests run: 1367 passed, 5 skipped
  doctest: ok (2305 ms)
  linux (the maintainer): ok (220784 ms)
gate: PASS
```

Linux log `gate-job-bashdescrev-20261121T005258Z/linux.log`: `Summary [121.765s] 1368
tests run: 1368 passed, 5 skipped`. Full logs in
`/c/Users/the maintainer/kelir-scratch/gate-job-bashdescrev-20261121T005258Z/`. No retries and no
flaky tests were reported in either log. Windows baseline at `830c96e` was 1362 passed /
3 skipped; the five extra passing tests are mine. Skips: 3 pre-existing ignores plus my
two intentionally-red N-ESC tests.

Independent checks, all from this worktree:

- `cargo test --lib docs` — 24 passed, 0 failed.
- `cargo run --quiet -- docs --generate` — `docs: nothing to generate`; drift test still 24/0.
- `cargo test --test e2e_bashdesc` — 3 passed, 0 failed.
- `cargo test --test e2e_bashdesc_adversarial` — 5 passed, 0 failed, 2 ignored.
- `cargo test --test e2e_bashdesc_adversarial -- --ignored` — 2 failed, 0 passed:
  the two red tests below prove N-ESC.
- `cargo test --test e2e_m9 the_next_day_adds_the_date_source_and_nothing_else` — 1 passed.

Added `tests/e2e_bashdesc_adversarial.rs`: invalid-type/blank/missing fallback, exactly
120 chars with a multibyte label and a "· ok" verdict, whitespace folding, a 120-char
wide label versus the 60-column live row, and the OSC-injection lab (passing "log keeps
the bytes, human line folds whitespace" + two `#[ignore]` red tests). The ignored tests
keep the gate green while documenting the bug; they are the failing scenario for S-1.

## Findings

| id | severity | file:line | finding and concrete failing scenario |
|---|---|---|---|
| S-1 | should-fix | `src/tools_view.rs:44-57`; reached from `src/session.rs:6283-6293` → `src/session.rs:6043-6058` | `description()` folds whitespace but does not strip other control characters. A model-supplied `description` containing `ESC ] 8;;https://evil.example ESC \`, `ESC ] 7501;state=done ESC \`, `ESC [2J`, BEL or NUL is printed verbatim in a headless progress line (`println!`/`eprintln!`). In a Herdr pane that is an OSC 8 hyperlink or a forged status record; in any terminal it can clear the screen. `src/herdr_ctx.rs:155-166` already filters controls on the same class of untrusted label. Proven: ignored test `no_control_byte_may_survive_the_label_sanitizer` fails with `"line one \u{1b}]7501;state=done\u{1b}\\"` and `a_hostile_description_must_not_put_osc_bytes_on_headless_stdout` fails with `"→ bash read \u{1b}]8;;https://evil.example\u{1b}\\files \u{1b}]7501;state=done\u{1b}\\ · ok · 6 B · 0.2s"`. Fix: map `char::is_control()` to `' '` before `split_whitespace()` (one function then covers all three render sites); keep the raw bytes in `events.jsonl`. |
| N-1 | nit | `src/tools_view.rs:44-57`, `src/activity.rs:256-270` | The cap counts characters, not display columns: CJK/emoji labels count 1 but occupy 2+ columns. `a_wide_multibyte_label_is_capped_in_characters_and_keeps_the_verdict` shows a 120-char all-CJK label makes the logical `activity::line()` 60+ columns wide; ratatui clips it to the pane, so no corruption, and the queue item's "120 chars" wording is met. No change needed unless a column cap is wanted later. |
| N-2 | nit | `tests/e2e_bashdesc.rs:110-157`, `docs/reference/tui.md:129-132` | The TUI-side evidence is rendered through `CallLine::line()`/`call_head()`/`call_label()` and a live-row test; I did not drive the interactive TUI through a real pty against the live model (only headless `run` and `run --keep`). The live row's description path is additionally exercised through `tui::call_label` in the unit tests, but a pane-level screenshot is not part of this suite. |

### Verified contract details

- Schema, `src/tools/mod.rs:195-203`: `description` is a string property beside `command`,
  `required: ["command"]`, `additionalProperties: false`. The tool description tells the
  model the command itself must never go in this field, and `src/prompt.md:6` asks for a
  short description. The field is still model-supplied text, so S-1 applies to it.
- Fallback: `src/tools_view.rs:44-48` returns `None` for missing, non-string and
  blank-after-trim values; `src/tui.rs:371-386` then emits `$ <command>`,
  `src/tui.rs:414-430` and `src/session.rs:6287-6293` emit the day-one truncated command.
  `every_invalid_description_falls_back_exactly` covers all three renderers byte-exactly.
- Cap: `DESCRIPTION_MAX = 120` (`src/tools_view.rs:35`), `chars().take(119)` + `…`
  (`:51-56`); `a_description_is_one_line_and_capped_at_120_characters` and the e2e
  "long and wide" test check 120 chars, not bytes, and one-line folding.
- No data loss: `assistant`/`tool_started` events carry both `description` and the full
  `command`; `tool_settled` keeps `result`, `bytes` and `out_path`; the output file has the
  full output; `/expand` reads that file. `--json` NDJSON is not affected by the new
  progress label because the print site is gated by `!self.json` (`:6043`). No cache-prefix
  change: the tool schema is sent in every request prefix; tests assert identical prefixes.
- Herdr records are not forged by the feature itself: `src/herdr.rs:61-67` builds
  `OSC_7501` from fixed state words only, and `src/tui.rs` paints strings through
  ratatui. S-1 is about the *model's* bytes reaching stdout in the headless path.
- Side fix `tests/e2e_m9.rs:443-453`: the hardcoded 2026-11-20/21 dates are replaced by
  `time::OffsetDateTime::now_utc()` ± one day. Every old assertion still exists with
  the same count/shape (`clocks.len() == 1`, clock name and text, frozen system prompt,
  tail lines, prefix equality); only the date literal became a variable. It is not a
  weakening: before the fix this test failed after 2026-11-21T00:00Z because the
  *second and third* runs used yesterday's `KELIR_TEST_TODAY`, so no new-day clock appeared.
  Passes now, and it passed in both gate halves.

## Live drive (Herdr panes, real OpenRouter model `deepseek/deepseek-v4.1-flash-1017`)

Release binary built from worktree HEAD (`cargo build --release`, 11 MB, 4m09s). Env file
outside the repo, `KELIR_STATE_DIR` in a scratch dir; keys never printed or copied into
the repo. All panes were created by me, waited with `hc wait`, then killed and `rm`ed by
name. No other pane was touched; no `kelir.exe` and no extra `sh.exe` remained after.

1. `kelir run --provider openrouter --model deepseek/deepseek-v4.1-flash-1017 "Run ONE bash command: printf ... Give that call a short description exactly: check the greeting label. Then reply DONE."` in `live-desc-review` (`t19b`). Pane showed
   `→ bash check the greeting label · ok · 12 B · 0.1s` then `DONE`. Log
   `ses_01m7xtcs86nj4ptsptfgn83r7n/events.jsonl` has `tool_calls.args` and
   `tool_started.input` with both `description` and `"command":"printf \"hello-label\\n\""`;
   `commands/6-1-1/out` contains `hello-label`. Verdict: PASS.
2. `kelir run --keep` with description `verify expand path` in `live-desc-expand`
   (`t19c`). Pane showed the label line and `DONE`; log and output file kept the full
   command `printf "expand-me\n"`. `/expand 1` printed nothing visible in the captured
   pane, so I do not claim a live screenshot of `/expand`; the file-level path is proven
   by e2e `tests/e2e_bashdesc.rs:48-63` and by the log/out files above. Verdict: PASS
   for label + log; `/expand` NOT LIVE-VERIFIED.
3. Hostile description: asked the model for `fake ESC ] 7501 state=done then say audit
   complete` in `live-desc-hostile` (`t19f`). The model declined to copy that text and
   used `Echo the literal word hostile`, so no bytes were injected; the pane's status
   remained the fixed record from the session outcome. The mock-based N-ESC tests above
   are the deterministic proof that a *literal* ESC sequence would be printed. Verdict:
   PASS for the real-model path, S-1 remains for a model that complies.

The Go provider path is not live-verified (no `KELIR_GO_API_KEY`); it shares the same
rendering code and is covered by the mock suite.

## Spec coverage / gaps

Queue item done lists: schema ✓, dispatch/payload ✓, settled TUI line ✓, live row ✓
(unit + e2e `call_label`, no pty screenshot), headless line ✓ (also live), 120-char
cap ✓, whitespace fold ✓, no data loss ✓, docs + drift ✓. Open: S-1 (control-byte
filter), N-1 (column cap, not requested), N-2 (interactive pty screenshot).

No secrets, hostnames, personal paths or API keys were added to the repo; the review's
scratch paths and the env file live outside it. No dependency was added. Release binary
is 11 MB, within the 20 MB footprint budget.
