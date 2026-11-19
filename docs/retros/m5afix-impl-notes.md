# m5afix — implementation notes (M5-A fixes)

Job: `docs/tasks/M5-A-fixes.md`. Branch `job/m5afix`, worktree `m5afix`. Commits
`m5afix: …` on that branch only, explicit paths.

## What I did

**The watcher (F1, F1b, F8).** `Tracked.hashes: Vec<String>` became a single
`hash`, the hash of the text the context *currently* carries (the frozen system's, or the
newest `source` message's). Comparing against one hash instead of "every hash ever carried"
is what makes A→B→A a message again, and it is the same rule the `skills` verb now uses
against the last `source` event of that name. A compaction that folds a `source` message
into a summary calls `Watch::forget_folded`, so the next boundary says what the file holds
even when its bytes did not change — live and on resume (the resume rebuild replays the
standing `Compacted` events too). `Watch::rebuild` replaced `from_sources` on resume: it
replays `sources[]` *and* every `source` event, so an edit the log already carries is not
announced twice and a verb-added source (path only in its event) keeps being watched.

**No sentinel paths (F2, F2b).** `SourcePath::Embedded | File(PathBuf)`; the log's path is
`""` for the embedded default, and an old log's literal `embedded` is read back as
`Embedded` only for the base prompt. `Config` gained `prompt_path`, so a `prompt = "file"`
records its real path. F2's second half: `load_prompt` no longer reads
`<process cwd>/.kelir/prompt.md` (the config does not know `--cwd`, and the render already
reads the session cwd's project prompt first).

**One decode rule (F3, F5, F6, F7, N5, N6).** `sources::read_source` is the single reader:
UTF-8 BOM dropped, invalid UTF-8 decoded lossily *with a note*, size capped (64 KiB per
file, 256 KiB total, `[kelir: truncated; N bytes on disk, the first M shown]`) with a note.
The render, the watcher and the `skills` verb all go through it and hash exactly those
bytes, so a skill added by `skills --path` is not announced twice and its `description`
(front matter) arrives. A malformed `SKILL.md` is skipped with the reason; a non-UTF-8 file
is kept; a deleted tracked source is one `error` (`where: "source"`), not silence (N4).
A file that does not fit the *total* budget is skipped, not half-read, so the watcher's
re-read reproduces the render's bytes.

**Discovery (F4, F9, N3).** Every `AGENTS.md`/`CLAUDE.md` on the walk up is kept, nearest
first; the walk stops at the repository root (a directory holding `.git`) and at home.
`sources[]` gained a `kind` (`prompt`/`instruction`/`skill`) recorded where the source was
found, so a skill directory called `notes.md` stays a skill. An old log without `kind` is
read back from the name.

**Tests (N2).** The ten `#[ignore = "FINDING …"]` repros run and pass. Two of them had
asserted the *old* behaviour and were corrected with a comment (the 3 MB file asserted
"no cap", F7; the edit-back test asserted "no duplicate", F1); nothing was loosened.
Four watcher tests now wait for `tool_started` instead of `tool_settled`: the watcher runs
at the boundary *after* the call settles, so writing the file once `tool_settled` was seen
raced the session's own poll (this showed up as one-in-fifteen failures before the change
and none in 5/5 after). New coverage: the compaction case live and on resume, the
whole-file skill body, a resume keeping a verb-added source watched, the deleted-source
note, malformed-skill notes, and the decode rule. The harness scrub now removes every
`KELIR_*` plus `TERM_PROGRAM`/`HC_TERMINAL_ID`, *before* the test's own env goes on, and
covers the pty spawns and the two commands `e2e_m4`/`review_m4` build by hand.

## Gates

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`
(all suites green) at `01289db`; Linux via
`bash scripts/remote-check.sh the maintainer kelir-verify-m5afix` — fmt clean, clippy clean,
**292 unit + every integration suite green**. Test counts (Windows single-threaded / Linux):
unit 287 / 292, `e2e` 16 / 16, `e2e_m3` 11 / 13, `e2e_m4` 10 / 8, `e2e_m5` 10 / 10,
`e2e_tui` 15 / 15, `e2e_verbs` 23 / 23, `review_m0..m4` 8/5/2/11/19, `review_m4b` 7,
`review_m5a` 20 / 20 (was 10 of 20), `review_steer1` 6 (+2 ignored from before this job).
The Windows/Linux differences in `e2e_m3`/`e2e_m4` are pre-existing `cfg` sets, not skips I
added.

Under a *fully parallel* `cargo test` on this box (several other kelir workers building at
the same time) single tests fail intermittently — ten runs in a row each had one or two,
in a different test each time: `e2e_verbs` (`model_swap_*`, `say_mid_turn_*`,
`a_headless_run_drains_*`, mostly `end was refused` / `the session did not answer`),
`e2e_m3` (`resume_refuses_a_session_that_is_still_running`), `review_steer1`
(`a_configured_window_wins_over_the_provider`), `review_m4` (`r18`/`r19` exec
grandchildren, `r13` bg wake), `e2e_m4` (`a_bg_run_inside_a_call_outlives_a_killed_session`)
and `e2e_tui` (`a_ctrl_c_before_the_first_message_does_not_stop_the_turn`). Every one of
them passed 6–12 of 6–12 in isolation, `cargo test --test <suite>` alone is green (5 of 5
for `e2e_verbs` and 20 of 20 for `review_m5a`), and a single-threaded full run is green
(three consecutive). `docs/reviews/M4-review-round2.md` ("Environment stalls") documents
this same class on this box: with other workers at load the `bg run` shim/console path and
the ctl socket timeouts degrade. None of the flaky tests touches a path this job changed
(model swap, bg jobs, session liveness, model windows) and none failed single-threaded.
The two `e2e_m5` edit tests that *did* have a real race with my changed code (writing the
file after `tool_settled`) were fixed rather than excused: they now write on
`tool_started`, and the suite then passed 15 of 15.

**Reviewer note:** if a gate run is red only in the parallel full-suite mode, re-run
`cargo test -- --test-threads=1` before reading it as a regression.

## What was hard

- **One rule for reading a file, applied in three places.** The F3/F5/N5/F7 fixes are all
  the same bug in different clothes: the render, the watcher and the verb each had their
  own read. The fix is a single `read_source`/`decode`; the subtle part was the *total*
  budget: truncating to the remaining room at render time produces text the watcher cannot
  reproduce (it reads each file with its own limit), which would announce the file again at
  the first boundary. Skipping a file that does not fit is the only rule that stays stable.
- **A spilled body in `latest_source_is`.** The "is this the same text as last time" check
  first read the log payload with `inline_str`, which is `None` once a body is big enough to
  spill to a blob — so a repeat of a large body slipped through. It resolves the payload
  now.
- **The `skills` verb never hashed the file.** It hashed the front-matter-stripped body;
  the watcher hashed the whole file. Making the verb send the whole file fixes F3, N5 and
  the `description` gap in one change, and drops `skill_body` entirely.
- **Test flakiness that looked like my bug.** `review_m5a`'s edit tests wrote the file right
  after seeing `tool_settled`, i.e. possibly after the session had already polled. Two
  failures in ~20 parallel runs. Waiting for `tool_started` (the scripted call sleeps a
  second) made them deterministic without touching what they assert.
- **Telling a load flake from a regression.** The parallel failures move around between
  suites and files, and reproduce neither in isolation nor single-threaded. I checked every
  one of them against the paths I changed (none: model swap, bg jobs, session liveness,
  model windows) before concluding they are the documented environment class.

## Where the prompt or tools slowed me down

- **`bash` argument length.** One `cat > file <<'EOF'` with a 400-line Rust module failed
  with `The filename or extension is too long (os error 206)` — the tool runs the command
  through `bash.exe -c "…"`, so the whole script is on the command line. I had to write the
  module as a series of `edit`/`python` calls instead. Worth a note in AGENTS.md: prefer
  `edit` or a heredoc from a file for anything over a few KB.
- **Python heredocs and backslashes.** Writing Rust string literals from a Python heredoc
  ate a `\` twice (a `'\'` character literal and a `"\n"`), each caught by the compiler.
  `edit` with an exact `old` string, or a `python -c` with a raw string, is the reliable
  route.
- **`#[ignore]` regex.** The first sweep missed the two attributes that contained escaped
  quotes around `"embedded"`; `grep` found them and a line-prefix filter fixed it.
- **The brief's "un-ignore them and make them pass without weakening them"** is slightly in
  tension with two of the ten: F7's test *characterised* the absence of a cap
  (`system.len() > 3 MB`) and F1's sibling in `tests/e2e_m5.rs` asserted "editing back is
  not a duplicate", which the F1 fix deliberately changes. I read "without weakening" as
  "keep proving the same property", and both now assert the *stronger* post-fix property
  (a cap with a visible marker; the edit-back said once, with the prefix unchanged), with a
  comment in place. The reviewer should check that reading.

## Files

`src/sources.rs` (most of the change), `src/session.rs` (watcher lifecycle, `apply_skills`,
`record_source`, `check_sources`, compaction hook, render warnings),
`src/event.rs` (`SourceInfo.kind`), `src/config.rs` (`prompt_path`, `load_prompt`),
`tests/support/mod.rs` (env scrub), `tests/review_m5a.rs`, `tests/e2e_m5.rs`,
`tests/e2e_m4.rs`, `tests/e2e_tui.rs`, `tests/review_m2.rs`, `tests/review_m4.rs`,
`docs/DEVIATIONS.md`.
