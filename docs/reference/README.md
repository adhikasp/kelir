# kelir reference

**Summary:** every knob kelir has, one topic per file, embedded in the binary so
the agent can read how to operate itself (`kelir docs`). Each file starts with
three lines: what it answers, when to read it, and where in the source it lives.
**Read this when:** you are about to configure, drive or change kelir and want
the whole surface in one place — or you are adding a knob and need to know what
else has to move with it.

## Reading it

- Inside a session (anywhere, no checkout needed): `kelir docs --list`,
  `kelir docs config`, `kelir docs --search max steps`.
- From the repo: the same files under `docs/reference/`.
- Each file's **source pointer** (`src/config.rs:load`) names the function that
  reads the knob, not a line number that moves: `git grep -n 'fn load'`.

## The files

| topic | answers |
|---|---|
| `README` | this index: how to read the reference |
| `config` | every `config.toml` key: type, default, override, effect, source |
| `cli` | every subcommand and flag, as printed by `kelir docs cli` |
| `env` | every environment variable kelir reads or sets |
| `verbs` | every `kelir ctl` verb: arguments, when it applies, what it writes |
| `mcp` | `mcp.toml`, `kelir mcp`, exit codes, the session broker, channel servers |
| `channels` | `kelir channel`, the bridge protocol, spool, fallback replies, `/stop` |
| `gates` | `[gates]`, skill-bound gates, the six events, allow/deny/rewrite, fail-open, the example gates |
| `jobs` | named sessions, `kelir run --name --single-flight`, the name lock, STATUS, scheduler recipes |
| `children` | `kelir run --child`, `brief`, `ls`, `kill`: children, caps, the tree, the trust model |
| `providers` | the two providers: key sources, retries, limits, model metadata |
| `sources-and-skills` | instruction files, skills, hints, memory, attachments |
| `files` | the state dir, a session dir, the event log, blobs, bg, locks, config search |
| `events` | every event type in `events.jsonl` and the fields to count on |
| `tui` | the interactive session: rows, keys, `/` commands, status states |
| `troubleshooting` | 429s, a paused turn, a stale socket, a lost job, Windows quirks |
| `source-map` | which source file owns what |

## Kept in step with the code

These files are hand-maintained markdown. The tables are written out as ordinary
text: no command rewrites them. A drift test (`cargo test --lib docs`) reads the
code and fails when a verb, a config key, an event type or an environment
variable exists there but is missing from this reference. The reverse (a
documented name whose code was removed) is caught at review.

## Adding a knob

1. Read the field, the flag or the verb where it is used.
2. Add its row to the right file here: `docs/reference/config.md`, `verbs.md`,
   `events.md` or `env.md`. A new environment variable also goes in `src/envs.rs`
   (the runtime accessor checks its name there).
3. Run `cargo test --lib docs`.
4. Update the *behaviour*, not just the name: the tables are prose now, so
   nothing edits them for you.
