# M5-B review (job m5b, `f02e5b3..6f9ff78`)

**Verdict: PASS WITH FIXES** — 0 blockers, 4 should-fix, 5 nits.

Every acceptance line of the brief holds on the real release binary. The headline (`observe --raw` equals the
wire body) held in every scenario I could throw at it. The fixes are about a credential going to a host it was
never meant for, a missing usage record, a task item that silently was not built, and a heavy-handed token cap.

## Test counts

| layer | result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --all-targets -- -D warnings` | clean |
| `cargo test --lib` | 586 passed |
| `cargo test --test e2e_m5b` (implementer) | 15 passed |
| `cargo test --test review_m5b` (mine, new) | 18 passed, 1 ignored (F1, fails when run) |
| Linux (`scripts/remote-check.sh the maintainer kelir-verify-m5b-rev`, HEAD 6f9ff78) | rc=0 |
| Windows `scripts/gate.sh` | not obtained: queued ~27 min behind other workers' gates on the machine lock and cancelled by me; I ran the pieces by hand instead (below) |

Release binary: 8.0 MB (budget 20 MB).

## Findings

### F1 — should-fix (security) — the session's API key is sent to `[hint] endpoint`
`src/session.rs:3192` (`provider.key = self.active_provider.key.clone()`) then `:3215`
(`Chat::new(&base, provider.key.clone(), …)`). When `hint.provider` is the session provider, the session's
key is attached to whatever URL `[hint] endpoint` names. A config of `endpoint = "http://127.0.0.1:8080/v1"`
(the docs' own example) sends the OpenRouter key to a local server; a shared project config could send it to
any host. Docs say only "held in memory only".
Failing test: `the_session_key_is_not_sent_to_a_different_hint_endpoint` (`tests/review_m5b.rs`, `#[ignore]`d
so the gate stays green; `cargo test --test review_m5b -- --ignored` shows
`Some("Bearer test-openrouter-key")` arriving at the second mock).
Fix direction: send the key only when `endpoint` is unset (or its host equals the provider's); otherwise
require an explicit `[hint] api_key_env`, or go keyless as the docs already say for local servers.

### F2 — should-fix — the hint call's cost/usage is not recorded
`src/hint.rs` `Chat::score` (about `:290`) never reads `usage` from the answer, and `maybe_hint`
(`src/session.rs:3108`) writes no usage event. Live: each scoring call used 39-111 completion tokens
(mostly reasoning) and about $4e-5 to $9e-5; over the 14-prompt drive that was $0.0006 against $0.0028 for
the 16 main calls, roughly 20% overhead, and none of it appears in `events.jsonl` (usage event kinds in the log:
`None` only). The brief asks for it to be recorded; cost accounting and `observe`'s cache ratio silently
exclude it.
Fix direction: parse `usage` and write a `usage` event with a `kind: "hint"` (`observe::cache_stats` already
filters on `kind: None`, so the ratio stays clean).

### F3 — should-fix — `kelir run --attach` / `ctl attach` from M5 item 6 were not built, and nothing says so
`docs/tasks/M5.md` item 6 names `kelir run --attach file…` and `ctl attach`. `src/cli.rs` has neither (only
`kelir view` / `ctl view`), and `docs/DEVIATIONS.md` does not mention it. Images, audio and video work through
`view`. PDF is refused (`view: …/paper.pdf is not an image kelir knows (png, jpg, gif, webp)`, test
`pdf_and_odd_media_names_are_handled_not_crashed`); the M5 text only requires audio/video, but the lead's brief
for this review said "audio/video/pdf stored with a note".
Fix direction: record the omission in DEVIATIONS (or add `--attach` as a thin loop over `view`), and decide
whether a PDF is a stored-with-a-note media kind (same `attachment-note` path) or a documented refusal.

### F4 — should-fix — scorer `max_tokens: 120` is tight for a reasoning model
`src/hint.rs:259`. The scorer model used reasoning: completion tokens per call were
`[111, 65, 72, 100, 43, 43, 63, 103, 82, 39, 79]` against a cap of 120. None truncated in 15 calls, but two
came within 10 tokens. A truncated answer is garbage and so silently "no hint". Same variance showed in
quality (see below, 9/10 on one run and 7/10 on the next).
Fix direction: raise the cap (for example 400), or ask for no reasoning where the provider supports it.

### Nits
- N1 `src/session.rs` `hint_note`/herdr note: a failed `hc` is an `error` event (`where: source`); the brief said
  "log a `source` note, never an error". Behaviour is fail-open as required; the event type is just noisier
  than asked (it can show as an error line in the TUI).
- N2 A deleted memory note is silent to the model (it keeps the last text). This is the M5-A design for every
  source (`src/sources.rs` "a deleted source is silent") but for memory, which the model curates, a stale note it
  cannot know is gone is more likely to mislead. A one-line tail message would fix it.
- N3 `observe` builds the request with the *current* config (`cache_ttl`, provider table) and the on-disk models
  cache, not the running session's. Editing the config after a session starts makes `--raw` differ from the next
  wire body. Untested; stated limit in the module doc covers only `refused_fields`.
- N4 `kelir ctl --help` lists the verbs as "say, later, stop, model, tools, end" and omits `observe` (and others).
- N5 Each human prompt waits for the scorer before its first step (up to the 2 s timeout). Fine for an opt-in
  feature; worth a sentence in the docs.

## What I verified (acceptance lines)

### 1. Herdr line
- Real pane (live): `herdr: pane t16q, space spcREV, tab tbREV, where "a device / kelir-build"` appears exactly
  once at the end of `# Environment`; it equals `hc get t16q --json` (address, `herdr.space`, `herdr.tab`,
  `where`). `sources[]` has `herdr` (kind `herdr`, empty path); `observe` lists it and prints the line.
  A pane with only `where` yields `herdr: pane t16p, where "…"`. Outside Herdr (`HC_TERMINAL_ID` removed): no line,
  no note. No agent count anywhere in the line.
- Fail-open on the mock (`tests/review_m5b.rs`): hang (bounded, process tree gone: `hc_hanging_is_bounded_near_one_second_and_leaves_no_process`),
  40 MB of junk stdout (`hc_printing_a_huge_answer_…`), a 5 MB label (`hc_valid_json_with_huge_labels_stays_one_bounded_line`),
  label with newlines, a `# heading`, ESC and NUL (`hc_labels_cannot_inject_lines_or_headings_…`), a flag-shaped
  `HC_TERMINAL_ID` (`a_pane_id_that_looks_like_a_flag_…`). The implementer's tests cover missing, erroring and
  malformed JSON.
- Windows, console-less parent (`launch_detached.py detached`, release binary, fake `hc.cmd` hanging on `ping -n 41`):
  the helper chain is `PING <- cmd <- kelir`; the only console host created for it hangs off the shim's `cmd.exe`
  (what `CREATE_NO_WINDOW` gives: a console with no window; `src/credentials.rs:450`); after kelir ended 0 helper
  processes were left. I did not take a screenshot of the desktop for this, so "no window" rests on the flag and
  the process tree, not on a pixel check.

### 2. Hint (live, `deepseek/deepseek-v4.1-flash-1017` via OpenRouter, through a logging proxy)
- Default off (e2e `the_hint_is_off_unless_configured`). Fail-open: scorer 500 / garbage / invented names /
  unreachable endpoint (implementer's e2e), slow scorer cut at about 2 s with one `error` note
  (`a_slow_scorer_costs_about_two_seconds_and_no_hint`), missing key (unit tests only: not live-verified here,
  an opencode credential store on this box would make a live run reach a real endpoint).
- The hint is a tail `source` message `might be relevant: name - description`; no skill body ever in it
  (`SIFT-BODY`/`CHANGELOG-BODY`/`CSV-BODY` absent, live and mock). One scoring call per human prompt, not per
  step, none on a resume (`one_scoring_call_per_prompt_across_steps_and_none_on_resume`).
- Prefix bytes: two live runs (14 and 11 main requests, 4 held-requests, an AGENTS.md edit, a memory edit, a
  model swap and a new memory note in between): request k+1 extended request k byte for byte in 13/13 and 10/10
  pairs. `cached_tokens` kept climbing across hints: 0, 4864, 4864, 4992, 4992, 5120 … (run 1, 90% overall,
  99% on the last step).
- Held-out prompts (3 skills `sift`, `changelog`, `csv-clean` plus the built-in `kelir` skill; 8 skill prompts
  and 2 with none expected, descriptions not echoed in the prompts): run 1 named the right skill (or nothing,
  where nothing was right) on **9/10**; run 2 on **7/10**. Misses were the paraphrased ones ("test output scrolled
  past my terminal buffer" → sift, "summarise what changed between v1.0 and v1.1" → changelog, "normalise the
  phone number column" → csv-clean): the scorer answered `[]`. No wrong skill was ever named. This is a quality
  note, not a gate: with a reasoning model and a tight `max_tokens` (F4) it is noisy.

### 3. Memory
Loaded as a `# Memory` section and `sources[]` (live: `GUAVA-2210` in the frozen system). Mid-session edits and
new notes arrive as tail `source` messages with the system and earlier messages unchanged (live, held-request
check, and `a_memory_note_the_model_writes_arrives_as_a_tail_source_message` for a model `edit`).
`memory_survives_delete_rename_huge_and_non_utf8`: a 300 KB note is bounded (system under 120 KB), a non-UTF-8
note is read lossily, a rename is a new note, a delete is silent (N2), the second request extends the first.
`memory_dir_missing_or_a_file_is_no_memory`. Symlink loop test is Unix-only (`memory_with_a_symlink_loop_…`), run
by the Linux gate; Windows could not create the links without privilege.

### 4. Attachments
Audio/video stored under the session with an `attachment-note` source (upper-case extension and a space in the
name too), never sent as bytes; a directory named `*.mp4` and a missing file are refused with an error. Image +
media note together: the image is sent once (one `data:image`), `observe --raw` equals the wire, the human view
does not dump base64 (`observe_equals_the_wire_with_an_image_and_a_media_note`). PDF: refused with a message (F3).

### 5. Observe
- Live, real provider, request held at the proxy so the comparison is not racy: `observe --raw` was byte-identical
  to the wire body after a hint, after an `AGENTS.md` plus memory edit, after a model swap and after a new
  memory note (4/4, `scripts/live/m5b_live.py` + `m5b_proxy.py`). My first, unheld attempt differed by one
  message only because the answer had landed before observe ran; that was my race, not a bug.
- Mock, request held in flight (`observe_equals_the_wire_*`): after a source change, a hint, a model swap, a
  retry (the retried body is also identical to the first), a compaction, and an image plus media note: all equal.
- Human view shows session/model, next request URL, token estimate, cache ratio (overall and last step), sources
  (frozen vs tail), hint, tools, system, messages. Works on a live session via `ctl <s> observe`, on a finished
  and a resumed one, and fails cleanly with a message and no panic on an empty, torn or garbage `events.jsonl` and
  on an unknown id (`observe_on_damaged_or_missing_sessions_fails_cleanly`).
- Auth: `observe` and `ctl observe` read `events.jsonl` directly, so there is no token and no `verb` event. It
  is read-only and documented (`docs/DEVIATIONS.md`). Anyone who can read the state directory already holds the
  same data, so I treat it as the existing same-user boundary. No Authorization header, no key in either output
  and none in `events.jsonl` (checked with the real key value).

### 6. Regressions
Base prompt 1023 bytes (limit 1024), unchanged. Sources/skills (M5-A) tests, STEER-5/7/12 suites and the
`docs/reference` drift test run in the lib/e2e suites and the gates.

## Live-drive log
- `scripts/live/m5b_live.py <scratch>` inside `hc run --bg --name live-m5b2 …` (env file sourced in the pane
  command, never printed): isolated state/config, proxy `scripts/live/m5b_proxy.py` (never logs headers).
  Outputs under `C:/Users/the maintainer/kelir-scratch/m5b-rev/` (outside the repo). Spend about $0.0035 per run.
- `scripts/live/launch_detached.py detached` probe for the console-less case (output above).
- Not live-verified: opencode-go path (no key), hint with a missing key.

## Added by this review
`tests/review_m5b.rs` (19 tests, 1 ignored), `scripts/live/m5b_live.py`, `scripts/live/m5b_proxy.py`.
No product code was touched.

## Gate
The machine-wide gate lock was held by other workers for the whole review, so the Windows `gate.sh` did not run.
By hand, one suite at a time on Windows: fmt, clippy `-D warnings`, `--lib` (586), `e2e_m5b` (15), `review_m5b`
(18 + 1 ignored), `e2e_m5` (10), `review_m5a` (20), `steer12` (5), `e2e_steer7` (11), `e2e_tui2` (2), doctests (0).
The full Linux half (`remote-check.sh`, fmt, clippy, whole nextest suite) passed, rc=0. The orchestrator should
run `bash scripts/gate.sh` on the branch before merging.
