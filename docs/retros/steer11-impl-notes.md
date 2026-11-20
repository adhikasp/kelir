# steer11 — self-documentation (impl notes)

Brief: `docs/tasks/STEER-11.md`. Branch `job/steer11`, worktree `steer11`, based on
`b7b60c6`, in small commits: the reference + `kelir docs` + registry + drift test;
the e2e file; the prompt line; the suggestion fallback; the spec/AGENTS text;
these notes; the `cli.md` and `sources-and-skills.md` prose; two test-message
fixes found by deliberately breaking the tables.

## What shipped

- **`docs/reference/`**: twelve topics (`README`, `config`, `cli`, `env`,
  `verbs`, `providers`, `sources-and-skills`, `files`, `events`, `tui` — that
  one was already there from TUI v2 —, `troubleshooting`, `source-map`). Each
  opens with a summary, a "read this when" and the source pointer. Every file is
  embedded with `include_str!`; `kelir docs <topic>` prints it, so a session with
  no checkout can read how to operate kelir. `gates-and-mcp.md` waits for M6/M10,
  as the brief says.
- **`src/envs.rs`**, the registry, and **every** `std::env` read in `src/`
  routed through it (config, paths, bg, herdr, ipc, session, shellcmd, theme,
  tui, credentials). A test walks `src/` and fails on any `env::var` outside the
  module, so a new knob cannot skip the docs.
- **`src/docs/tables.rs`**: verbs (10), config keys (30, top level and
  `[providers.<name>]`), event types (16); **`src/envs.rs`**: 31 variables.
- **`kelir docs`**: `[topic]`, `--list`, `--find`, `--generate`, `--check`.
  Five topics carry a block between `<!-- generated:kind -->` markers that
  `--generate` rewrites from clap/`VARS`/the tables; the prose is untouched and
  an unchanged file is not rewritten.
- **`src/docs/tests.rs`**: the drift test, which is stricter than `--check`
  because it reads the *code*: the arms of `verbs::parse`, the fields of
  `RawConfig`/`RawProvider`, the arms of `EventBody::event_type()`, and every
  quoted `KELIR_*`/`HC_*` literal anywhere in `src/`, compared as sets with the
  tables in both directions. Plus: every file on disk is a topic and every topic
  is a file, the embedded copy equals the file, each rendered block reaches its
  topic, generation is idempotent, an unknown/unclosed marker is an error, the
  CLI walk covers every visible subcommand (and never a hidden one), the search
  ranks, and the size budget (128 KB) is reported.
- **The builtin `kelir` skill** (`src/docs/skill-kelir.md`, no file on disk):
  `SourcePath::Builtin` is the typed absence of a path, the skill is discovered
  at the machine layer (so a project skill of the same name replaces it), and it
  names `kelir docs <topic>`, every topic, and the source map.
- **The base prompt** carries one line naming `kelir docs` inside the 1 KB
  budget (1013 bytes with it).
- **`tests/e2e_steer11.rs`**: eight tests through the real binary from a cwd with
  no `docs/reference`.

## What was hard

- **The 1 KB prompt budget.** The old prompt was 1015 bytes; one more sentence
  did not fit. The line that went in is 67 bytes, so 67 bytes had to leave: the
  wording got tighter (`several hunks, all or none → hunks, all or none`,
  `sed -n a,bp`/`git grep` folded into one line). Every line pinned by
  `sources::tests::the_embedded_prompt_carries_every_retro_rule` still matches,
  and the snapshot moved in the same commit as its test demands. If the budget
  is genuinely too small for the next line, that test is where to argue about
  it, not in a commit that quietly grows it.
- **The drift test is where the missing prose shows up.** The first run failed
  on things no generator could have caught: `view` was documented as applying at
  `next_step` while `Verb::scope` says `now` (the event's `boundary` is what a
  reader sees, so the table now says `now` and the prose explains the
  difference), `KELIR_DOCS_ROOT` was read by `docs` but only through
  `envs::named` (the "any name the config chose" escape hatch) and so had no
  row, and `bg` is a verb `parse` accepts but whose args are a job status file,
  so the table-parsing helper has to skip it rather than call it undocumented.
  Each of those was a real bug in one of the two sides, not a test that needed
  loosening.
- **Telling a marker from prose about markers.** `README.md` documents the
  `<!-- generated:kind -->` format inside a fenced block, and the first version
  of `regenerate` treated that as a block to rewrite (it ate the rest of the
  file). A marker is now a line that is *exactly* the marker: `marker_kind` takes
  the prefixes and suffixes and nothing else.
- **`--generate` in a test must not write into the checkout.** The e2e copies
  `docs/reference` to a temp dir and points `KELIR_DOCS_ROOT` at it, so a test
  can tamper with a row, watch `--check` fail, watch `--generate` fix it, and
  leave the repo's own files alone. That is also why `KELIR_DOCS_ROOT` had to be
  a declared variable rather than `named`.

## Where the tools slowed me down

- **A drift test that only ever passes proves nothing.** Every direction was
  checked by breaking the tables on purpose and reading the failure: delete a
  verb row (`end`), add a row the code does not parse (`frob`), rename a config
  key, rename an event type, declare a variable nothing names. All five fail
  with a sentence that says which side to fix; the first version of the config
  message interpolated an empty scope string ("a field of `RawConfig` ()"),
  which is how those two message fixes got in.
- **`edit` with a large `edits` array:** one hunk whose context had drifted
  applied nothing (all-or-none, as documented) and the build then failed with
  `E0599` a long way from the cause. The fix was a `python` replace, which is
  exactly what the prompt tells me not to reach for — the honest lesson is that
  a six-hunk edit against a file I have only read in slices is worth splitting.
- **`cargo test --lib docs::` does not match `src/docs/tests.rs` tests** until
  the module is declared: the filter silently ran 0 tests and `tail` showed
  "ok". A stale `mod tests;` was the bug, but "0 tests, 0 failed" is a shape
  that reads like success. Filter with the bare test name.
- **The first Linux run failed on the prompt and the e2e, both because I had
  committed the code before the prompt edit.** `remote-check.sh` bundles
  committed `HEAD`, so the far side tested the old prompt against the new
  budget test. Committing the prompt with the code that needs it (one commit,
  not two) would have saved a 90 s round trip.
- **`env::var` was reachable in a dozen files** before `envs.rs` existed, so the
  first pass was mechanical and wide (13 files, 4 lines each). That is the shape
  of change a reviewer finds hardest to read; keeping the doc sentence next to
  the declaration (`VARS` rows carry their own effect text) is what makes it
  reviewable in one diff.

## Known gaps

- `gates-and-mcp.md` is not written: no gates or MCP servers exist yet (M6/M10).
- `--find` is a line-and-term ranking, not full-text search: it answers "which
  file do I open", and the reference says so.
- The size budget is a test, not a gate: 128 KB for the embedded reference, with
  the binary's own 20 MB budget nowhere near it.
