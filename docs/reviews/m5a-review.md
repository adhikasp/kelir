# M5-A review — sources and skills

Reviewer: fresh QA agent. Diff: `7f27adf..ec1c54e` on `main` (code in `ccd556b`, `65e6509`, `9826dbe`; docs after).
Brief: `docs/tasks/M5-A.md`. Scope: base prompt, instruction files, skills dirs and format, frozen system render,
`source` events and the `skills` verb, `sources[]`. Not reviewed (M5-B): pushed hint, memory, attachments, `observe`.

## Verdict: **PASS WITH FIXES** — 0 blockers, 11 should-fix, 6 nits

Every acceptance line of the brief holds on Windows and Linux, the cache prefix is byte-stable across a source
change both against the mock and against a real model, and no gate is red. The fixes below are real defects (all but F7 have a failing test in `tests/review_m5a.rs`, `#[ignore = "FINDING …"]`; run `cargo test --test review_m5a -- --ignored`),
but none loses data, breaks the prefix, or blocks M5-B. The ones worth doing before M5-B builds on the `Watch`:
**F1, F2b, F3, F8** (all in the watcher's bookkeeping).

## Gates

| Gate | Windows | Linux |
|---|---|---|
| `cargo fmt --check` | clean | clean |
| `cargo clippy --all-targets -- -D warnings` | clean | clean |
| `cargo test` at `ec1c54e` | all green (below) | all green |
| unit (`--lib`) | 280 | 285 |
| `e2e` / `e2e_m3` / `e2e_m4` / `e2e_m5` | 16 / 11 / 10 / 6 | 16 / 13 / 8 / 6 |
| `e2e_tui` / `e2e_verbs` | 15 / 23 | 15 / 23 |
| `review_m0` / `m1` / `m2` / `m3` | 8 / 5 / 2 / 11 | 8 / 5 / 2 / 11 |
| `review_m4` / `m4b` / `steer1` | 19 / 7 / 6 (+2 ignored) | 19 / 7 / 6 (+2 ignored) |
| **`review_m5a` (mine)** | **10 pass, 10 ignored (all 10 FINDING repros fail when run)** | 10 pass, 10 ignored |

The Linux gate ran `scripts/remote-check.sh <linux box> kelir-verify-m5a-rev` (private work dir). After my own
commit the Linux run of `review_m5a` exposed a race in my first draft of one test (a resume already runs a turn by
itself; my extra `say` produced extra requests that Windows happened to miss); fixed in the final file.
`tasklist` after all runs: no `kelir.exe`, no logging-proxy `python`, no leftover bash of mine. Junctions made by the
loop test are removed with the tempdir. Release binary 6.4 MB; no new dependency (`Cargo.toml`/`Cargo.lock` untouched).
Public-repo scrub of the diff and my files: no usernames, hostnames or home paths.

## Acceptance lines of the brief

| Line | Result | Evidence |
|---|---|---|
| Base prompt ≤ 1 KB, embedded, overridable by machine and project | PASS | `src/prompt.md` is 704 bytes; unit test; `base_prompt_layers_and_the_skills_placeholder` (machine `prompt.md`, project `.kelir/prompt.md` wins, a prompt without `{{skills_dirs}}` still gets the dirs appended) |
| Instruction files walk up, nearest first, project over machine of the same name | PASS for the letter; see F4 (an ancestor's file is dropped) | `project_wins_over_machine_and_the_nearest_file_wins` |
| Skills dirs machine + project, Agent Skills `SKILL.md` | PASS; see F6 (malformed skills are not rejected) | same test, `awkward_files_are_tolerated` (hidden dir, no `SKILL.md`, stray file all skipped) |
| Render once, `system` + hash, `sources[]`, date never time, byte-identical | PASS | `the_system_render_is_byte_identical_and_has_no_time` (two sessions: same system bytes, hash, `sources[]`; no `dd:dd:dd`); live run |
| Mid-session change = one `source` event at the tail, no system rewrite | PASS | `an_edited_instruction_file_…` (theirs), `cache_prefix_only_grows_across_two_source_changes` (mine: 4 requests, AGENTS.md edited twice and a skill once, every request extends the previous byte for byte, exactly 3 `source` events) |
| `skills` verb adds or replaces, lands at the next boundary | PASS; see F1b, F3 | their `the_skills_verb_adds_and_replaces_a_source`; verb is in the log with `boundary: next_step` |
| Skills are pull: dirs named, no listing, no descriptions | PASS | system contains no body, no skill name, no description (asserted); live run |
| `sources[]` carries `{name, path, hash}` of the real files | PASS | e2e + live |
| Editing back does not duplicate | PASS by the letter; semantically wrong, see F1 | their test; my `editing_a_file_back_…` |
| Nothing is read outside the intended dirs | PASS | `nothing_outside_the_intended_places_is_read`: sibling dir, child dir, an ancestor's `.claude/skills` and `.kelir/prompt.md`, `~/skills`, `~/CLAUDE.md`, the config dir's parent are all inert. (By design the walk-up reads every ancestor's `AGENTS.md`/`CLAUDE.md` up to the filesystem root: N3.) |

## Findings

Severity: blocker / should-fix / nit. Line numbers are at `ec1c54e`.

| # | Sev | Where | Finding | Failing test |
|---|---|---|---|---|
| F1 | should-fix | `src/sources.rs:644` (`tracked.has`), `:534` | **Edit A→B→A leaves the model told B.** `Tracked.hashes` is "every hash ever carried", so going back to A is "already in the context" and emits nothing, but the *latest* message the model saw about the file says B. Scenario: AGENTS.md says `ORIGINAL-RULE`; edited to `TEMPORARY-RULE` (source message appended); edited back; the next request's last word on the file is still `[source: instruction AGENTS.md]\nTEMPORARY-RULE`. The model acts on a rule that is no longer on disk. The brief's "does not duplicate" is satisfied by comparing against the *most recent* carried hash per source, not all of them; A→B→A should append A (it is not a duplicate of the tail). Same applies after a `compact` that folds the B message away. | `editing_a_file_back_leaves_the_model_with_the_current_text` |
| F1b | should-fix | `src/session.rs:1493-1503` (`reused`) | **The `skills` verb has the same flaw and refuses it with an error.** `skills --text A`, `--text B`, `--text A`: the third is rejected ("already carries exactly this text") because an *earlier* message has A; the model keeps B. The doc comment on `apply_skills` (`:1443`) says the opposite ("replaces it, even when byte-identical"). Compare with the *last* source event of that name only. | `the_skills_verb_can_restore_an_earlier_body` |
| F2 | should-fix | `src/session.rs:2828-2843` (`prompt_path`), `src/config.rs:372` | **A base prompt from config `prompt = "file"` is recorded in `sources[]` with `path: "embedded"`** (`prompt_path` only recognises the config dir's `prompt.md`). Log readers and `observe` (M5-B) cannot tell it from the default, and a second wrong provenance case: `load_prompt` (`config.rs:386`) looks at the *process* cwd's `.kelir/prompt.md`, not `--cwd`. | `an_explicit_prompt_file_is_recorded_with_its_path` |
| F2b | should-fix | `src/sources.rs:594` (`Watch::add`), `:637-642` (`changes`) | **`embedded` is a sentinel path that the watcher reads as a real relative file.** With the embedded default prompt, `Tracked.path = "embedded"` and every boundary does `read("embedded")` relative to the process cwd. Any file called `embedded` in the cwd (an embedded-systems repo is plausible) becomes a bogus `source` event: `kind: instruction, name: prompt.md, text: <that file>`, i.e. an unrelated file injected into the context as if it were the base prompt. Reproduced. Fix: give embedded sources an empty path (the watcher already skips empty paths, `:637`). | `a_file_named_embedded_is_not_mistaken_for_the_base_prompt` |
| F3 | should-fix | `src/session.rs:1482-1487, 1502-1505` vs `src/sources.rs:643` | **A skill added by `skills --path <dir>` is announced twice.** The verb hashes the front-matter-stripped body (`skill_body`) and registers that hash; the watcher hashes the whole `SKILL.md`. The first boundary after the verb finds a "new" hash for an unchanged file and appends a second `source` with the full text. (A skill already discovered at start is unaffected because its full hash is already known, which is why their tests miss it.) Hash the file text, not the stripped body. Also N5. | `a_skill_added_by_path_is_announced_once` |
| F4 | should-fix | `src/sources.rs:216-241` (`push_source`, `Some(_) => {}`) | **Only the nearest `AGENTS.md` and the nearest `CLAUDE.md` are kept; an ancestor's file is dropped.** The brief says "walk up … and *collect* `AGENTS.md` and `CLAUDE.md` (nearest first …)", and "nearest first" only means something with several files. Scenario: monorepo root `AGENTS.md` (house rules) plus `sub/AGENTS.md`, session in `sub`: the root rules never reach the model. Dedupe by name should only apply across layers (project over machine), not within the project walk. The implementer read this deliberately (`impl-notes`: "first nearest wins"); I think it is a misreading. If you disagree, say so in `DEVIATIONS.md`. | `an_ancestors_instruction_file_is_collected_too` |
| F5 | should-fix | `src/sources.rs:270-275` (`read`) | **A non-UTF-8 `AGENTS.md`/`CLAUDE.md` is silently dropped** (`read_to_string(...).ok()`): not in the prompt, not in `sources[]`, no warning. A Latin-1 or UTF-16 file from a Windows editor vanishes without a trace, and the same code path makes a mid-session edit to non-UTF-8 invisible. Use a lossy read (and keep the BOM out), or record an `error`. | `a_non_utf8_instruction_file_is_not_silently_dropped` |
| F6 | should-fix | `src/sources.rs:244-268` (`skills_in`), doc at `:301-303` | **Discovery never calls `parse_skill`.** The `parse_skill` doc says a file without front matter or `name` "is skipped with a reason, never rendered", but `skills_in` accepts any `*/SKILL.md`. A skill with no front matter, an unclosed fence, or no `name` is listed in `sources[]` (and watched). Only the `skills` verb validates. Skills are pull, so the model is not misled by the prompt, but `sources[]` claims they are skills. | `a_malformed_skill_is_not_recorded_as_a_source` |
| F7 | should-fix | `src/sources.rs:165-211`, `:439-450` | **No size cap on anything that goes into the prompt.** A 3 MB `AGENTS.md` is inlined whole (system prompt 3.1 MB; the run still works, `a_huge_instruction_file_still_starts_and_is_recorded`), re-read and re-hashed at every boundary, and a later edit re-sends the whole 3 MB as a `source` message. A 30 MB file would blow the model's window on step 1 with no hint why. Needs a cap with a visible truncation marker (and a note in `sources[]`/an `error`), like the spill limit for tool output. Not blocking: the footprint rule is about idle RSS, and the file is the user's. | characterised, passes |
| F8 | should-fix | `src/session.rs:918-925` (`Watch::from_rendered` only), `reopen` | **A resume re-announces an edit its own log already carries.** The resumed `Watch` is rebuilt from `session_started.sources[]` only; the `source` events in the log are never replayed into it (`note_text` is only called while recording). Scenario: AGENTS.md edited mid-session → `source` event (1). Session killed, resumed with the file unchanged → a second, identical `source` event (2) appended at the first boundary. The impl-notes claim "reports the edits a live session would have"; it also reports ones it already did. Verb-added sources (path only known from the `source` event) are lost on resume for the same reason. Replay the log's `source` events into the watch in `reopen`. | `a_resume_does_not_repeat_a_source_the_log_already_has` |
| F9 | should-fix | `src/sources.rs:512-519` (`kind_of`) | **`kind_of` guesses a source's kind from its name**: anything ending in `.md` is an instruction, `prompt.md` is the base. A skill directory called `notes.md` (legal: `[A-Za-z0-9._-]`) is tracked as an instruction, so its mid-session edit arrives as `kind: instruction`. Store the kind in `sources[]` or key the watch off the discovered `Found.kind`. Low odds, cheap fix. | `a_skill_named_like_a_markdown_file_stays_a_skill` |
| N1 | nit | `src/session.rs:1443-1445` | Doc comment of `apply_skills` contradicts the code (see F1b). | — |
| N2 | nit | `tests/support/mod.rs:1146-1163` | The env scrub covers four variables. The binary also reads `KELIR_PROMOTE_AFTER`, `KELIR_HERDR`, `KELIR_TEST_CRASH_AT`, `KELIR_TEST_PANIC`, `TERM_PROGRAM`, `XDG_RUNTIME_DIR`; an ambient `KELIR_PROMOTE_AFTER` in a developer shell would still change `e2e_m4` behaviour. Prefer removing every `KELIR_*` and then setting what the test wants. |
| N3 | nit | `src/sources.rs:186-201` | The walk goes to the filesystem root with no stop at a repo root or home. On a shared Linux box a planted `/tmp/AGENTS.md` is picked up by every session whose cwd is under `/tmp`. Matches what the spec says (walk up), but worth a `DEVIATIONS`/doc line, or stopping at the home directory. |
| N4 | nit | `src/sources.rs:634-656` | Known gap confirmed: a deleted source is silent (`a_deleted_source_is_quiet_and_a_recreated_one_is_reported` pins this and the recreate path). Also silent: a skill or instruction file *created* mid-session (not tracked), and a file that becomes non-UTF-8. The model can still find new skills (the dirs are named), so I rate these nits. |
| N5 | nit | `src/session.rs:1482` vs watcher | The verb path sends a skill's body without front matter, the watcher sends the whole file; a skill's `description` therefore arrives in one case and not the other. Pick one. |
| N6 | nit | `src/sources.rs:442-449` | A UTF-8 BOM at the start of `AGENTS.md` is kept inside the system text (U+FEFF mid-prompt); CRLF is kept as-is. Deterministic, harmless, but strip the BOM. Also a prompt always names three or four skills dirs that may not exist (known gap); ~250 bytes, and it makes the prefix machine-specific (the `review_steer1` change to `start_reusing` was needed for that), acceptable. |

## Judgement on the implementer's two hazards

* **Tests inheriting `KELIR_SESSION_DIR` / `KELIR_SOCK` / `KELIR_CALL_ID` / `KELIR_BIN`: the fix is sound.** All spawn
  sites go through `tests/support` (scrubbed in `run_task`, `Sandbox::kelir_command`, `live_command`) or carry their
  own `env_remove` (`e2e_m4.rs:611`, `review_m4.rs:366`). I verified by running `e2e_m4`, `review_m4` and `e2e_m5` with
  all four variables set to bogus values: 10 / 19 / 6 green. Residual: N2. `KELIR_HOME` is set in every harness,
  including the PTY ones (`e2e_tui`, `review_m2`), so no test reads a real `~/.claude`.
* **"r18 marker scan polluted by its own lister":** I find no change for it in `7f27adf..ec1c54e`; `pids_with` in
  `review_m4.rs:785` (unchanged since M4-review) splits the marker in two halves and excludes `powershell.exe`, so the
  lister cannot match itself, and `pgrep -f` excludes itself on Linux. `r18` passed 3 more times in a row (7.3 s each)
  and in both full runs. If the fix lives in a branch I was not shown, I could not review it.

## Live drive (real binary, Herdr pane, `deepseek/deepseek-v4.1-flash-1017` on OpenRouter)

Script: `scripts/live/m5a_live.sh <scratch>` (isolates `KELIR_HOME`/`KELIR_CONFIG_DIR`/`KELIR_STATE_DIR` in the scratch
dir so no real `~/.claude/skills` or personal file can reach the provider; a pass-through logging proxy records request
bodies, never headers). Command: `hc run --bg --name live-m5a … bash -c 'set -a; . <env file>; set +a; [PAD=1] bash scripts/live/m5a_live.sh <scratch>'`.
Scratch project: `AGENTS.md` ("project codeword is PAPAYA-4417"), skill `sift` ("The sift code is SKILL-CODE-9031").
Task: state the codeword, run `sleep 10`, read the `sift` skill and report its code; during the sleep the script rewrites
`AGENTS.md` to `MANGO-8802`; afterwards `kelir ctl latest say` asks for the codeword again.

| Run | Observed | Verdict |
|---|---|---|
| A (small prompt) | `sources[]`: `prompt.md` (embedded), `AGENTS.md`, `sift`, each with a path and 16-hex hash. System holds `PAPAYA-4417` and not the skill body. One `source` event (`instruction`, `AGENTS.md`, `…MANGO-8802…`). The model answered the codeword from the system/source message, read `SKILL.md` with `bash` and reported `SKILL-CODE-9031`. 4 requests; each extends the previous one **byte for byte**, system message identical. `cached_tokens` 0 on every step (prompts only 1.0–1.7k tokens, under the provider's caching threshold). | PASS |
| B (`PAD=1`, AGENTS.md padded to ~11 KB so the prompt is ~3.7k tokens) | Same shape; 6 requests, each extends the previous byte for byte, system same. `cached_tokens`: **0, 0, 3584, 4096, 4096, 4352** — keeps climbing across the mid-session edit, i.e. the source message did not break the cache. Answer after the edit: `MANGO-8802`; second turn: `MANGO-8802`. (Side finding: at 11 KB the `session_started.system.text` is stored as a spilled blob; resume reads it back whole, `a_resume_keeps_a_system_prompt_that_was_spilled_to_a_blob`.) | PASS |

Spend: two short runs, ~0.01 USD. The Go path is not live-verified (no key); mock only.

## Tests I added

`tests/review_m5a.rs` (20 tests; 10 pass, 10 ignored FINDING repros F1, F1b, F2, F2b, F3, F4, F5, F6, F8, F9),
`scripts/live/m5a_live.sh`. Passing ones: project-over-machine and nearest-wins; outside-dirs decoys; byte-identical
render and no clock; CRLF + BOM + invalid UTF-8 + hidden dir + missing `SKILL.md` + stray file; junction/symlink loop
in a skills dir and through the cwd (Windows junctions and Unix symlinks, both cleaned up); base prompt layers and the
placeholder; 3 MB instruction file; deleted then recreated source; cache prefix growing across three source changes;
resume with a spilled system.

## Spec items not covered by this slice (so not failures)

The Herdr line (`HC_TERMINAL_ID` → one line in the frozen system) is listed under *Sources* in the spec and M5's table,
but not in `docs/tasks/M5-A.md`; it is absent and should be in M5-B's brief. The pushed hint, memory, attachments and
`observe` are M5-B by the brief.
