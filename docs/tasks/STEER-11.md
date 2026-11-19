# STEER-11 — self-documentation: what is configurable, readable by kelir itself

Maintainer requirement (2026-11-19): a docs set that exposes **every user-configurable tweak** so the kelir agent can
read how to operate itself, with pointers into the source for depth. Organised, and part of the injectable steering
rules / builtin skills when those exist (M5-B hint, M8 skills).

## Build
1. **`docs/reference/`** (shipped with the repo, organised, one topic per file, each starting with a 3-line summary and
   "when to read this"): `README.md` (index + how to read it), `config.md` (every `config.toml` key: type, default,
   env/flag override, effect, **source pointer** `src/config.rs:LINE`), `cli.md` (every subcommand and flag, from clap),
   `env.md` (every environment variable kelir reads or sets: `KELIR_*`, `HC_*`, provider keys), `verbs.md` (every ctl
   verb: args, applies-at boundary, event written), `providers.md` (the two providers, key sources, retry/limits,
   model metadata, price table), `sources-and-skills.md` (instruction files, skills dirs, hint, memory, attachments),
   `files.md` (state dir layout, session dir, events log, blobs, bg, locks, attachments, config search order, per OS),
   `events.md` (event types and fields), `gates-and-mcp.md` (as they land), `tui.md` (keys, commands, status states),
   `troubleshooting.md` (429, step limit, stale sock, lost jobs, windows console quirks, secrets handling), `source-map.md`
   (which file owns what, with pointers).
2. **No drift**: generate what can be generated (`kelir docs --generate` writes cli.md/verbs.md/env.md fragments from
   code: clap, the verb table, an env-var registry in one Rust module that all env reads go through) and add a
   **drift test** that fails when a config key, CLI flag, verb, env var or event type exists in code but not in the docs
   (and the reverse). Hand-written prose lives between generated markers.
3. **Readable by the agent**: `kelir docs [topic]` prints a reference topic (embedded in the binary with `include_str!`,
   so it works without the repo), `kelir docs --list`, `kelir docs --find <words>` (simple ranked search). The base prompt
   (`src/prompt.md`, <= 1 KB budget: one line) says "run `kelir docs` to learn how to configure or operate kelir".
4. **Builtin skill `kelir`** (Agent Skills format, embedded, listed like any skill by the sources code): description =
   "operate or configure kelir itself: config keys, CLI, verbs, providers, files"; body points to `kelir docs <topic>` and
   the source map. The pushed hint (M5-B) and steering rules (M8) can name it. It must be loadable when the repo
   is not present.
5. Update SPEC.md with a short "Self-documentation" section; each future milestone's brief must say "update
   docs/reference" in its Done list (add this line to AGENTS.md).

## Tests
Drift test (code <-> docs both ways), `kelir docs` output tests (topics, unknown topic suggestions, find), generation
idempotence (running `--generate` twice changes nothing; CI check mode `--check`), the builtin skill present in the
rendered sources with the right description and no repo on disk, size budget of the embedded docs reported. Windows +
Linux gates; no leaked processes.
