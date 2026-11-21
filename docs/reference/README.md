# kelir reference

**Summary:** every knob kelir has, one topic per file, embedded in the binary so
the agent can read how to operate itself (`kelir docs`). Each file starts with
three lines: what it answers, when to read it, and where in the source it lives.
**Read this when:** you are about to configure, drive or change kelir and want
the whole surface in one place — or you are adding a knob and need to know what
else has to move with it.

## Reading it

- Inside a session (anywhere, no checkout needed): `kelir docs --list`,
  `kelir docs config`, `kelir docs --find max steps`.
- From the repo: the same files under `docs/reference/`.
- Each file's **source pointer** (`src/config.rs:load`) names the function that
  reads the knob, not a line number that moves: `git grep -n 'fn load'`.

## The files

| topic | answers |
|---|---|
| `README` | this index: how to read the reference |
| `config` | every `config.toml` key: type, default, override, effect, source |
| `cli` | every subcommand and flag, generated from clap |
| `env` | every environment variable kelir reads or sets |
| `verbs` | every `kelir ctl` verb: arguments, when it applies, what it writes |
| `mcp` | `mcp.toml`, `kelir mcp`, exit codes, the session broker, channel servers |
| `channels` | `kelir channel`, the bridge protocol, spool, fallback replies, `/stop` |
| `gates` | `[gates]`, skill-bound gates, the six events, allow/deny/rewrite, fail-open, the example gates |
| `jobs` | named sessions, `kelir job run`, `jobs.toml`, single-flight, STATUS, scheduler recipes |
| `children` | `kelir run --child`, `brief`, `ls`, `kill`: children, caps, the tree, the trust model |
| `providers` | the two providers: key sources, retries, limits, model metadata |
| `sources-and-skills` | instruction files, skills, hints, memory, attachments |
| `files` | the state dir, a session dir, the event log, blobs, bg, locks, config search |
| `events` | every event type in `events.jsonl` and the fields to count on |
| `tui` | the interactive session: rows, keys, `/` commands, status states |
| `troubleshooting` | 429s, a paused turn, a stale socket, a lost job, Windows quirks |
| `source-map` | which source file owns what |

## Generated, and checked

Five topics carry a generated block between markers:

```text
<!-- generated:cli -->   … clap's own model   <!-- /generated -->
<!-- generated:config --> … src/docs/tables.rs <!-- /generated -->
<!-- generated:env -->   … src/envs.rs        <!-- /generated -->
<!-- generated:events --> … src/docs/tables.rs <!-- /generated -->
<!-- generated:verbs -->  … src/docs/tables.rs <!-- /generated -->
```

The prose around the markers is hand-written and is never touched. Regenerate
with `kelir docs --generate` (it needs a checkout); fail a build with
`kelir docs --check`. The drift test (`cargo test --lib docs`) is stricter: it
also reads the *code*, so a config key, a CLI flag, a verb, an environment
variable or an event type that exists in one and not the other is an error in
both directions.

## Adding a knob

1. Read the field, the flag or the verb where it is used.
2. Put the name and its one sentence in the data: `src/envs.rs` (a variable),
   `src/docs/tables.rs` (a key, a verb, an event).
3. Run `kelir docs --generate`, then `cargo test --lib docs`.
4. Update `docs/reference/<topic>.md` if the *behaviour* changed, not just the
   name: the tables are generated, the explanation is not.
