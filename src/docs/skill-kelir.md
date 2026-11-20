---
name: kelir
description: operate or configure kelir itself (config keys, CLI, verbs, providers, files, the event log)
metadata:
  builtin: true
  body: kelir docs
---

# kelir: the harness you are running in

You are a session of kelir, a small autonomous agent harness. This skill is about
kelir itself, not about the task you were given: read it when the task is
"configure this session", "why did it do that", "what can I steer", or when a
knob, a flag or an event type is what stands between you and the work.

## The reference is inside the binary

`kelir docs` works with no checkout on disk:

```sh
kelir docs --list                 # the topics, one line each
kelir docs config                 # every config.toml key: type, default, override, effect
kelir docs --find retry max wait  # ranked search: which file mentions these words
```

Topics: `README`, `config`, `cli`, `env`, `verbs`, `mcp`, `gates`, `children`, `providers`,
`sources-and-skills`, `files`, `events`, `tui`, `troubleshooting`, `source-map`.

## The five things worth knowing by heart

- **This session is a process plus a log.** `<state>/sessions/<id>/events.jsonl`
  is the only source of truth: every request is `fold(events.jsonl)`, live and on
  resume. A file under `commands/<call-id>/out` holds a tool call's whole output.
- **Everything is a verb.** `say`, `later`, `stop`, `model`, `tools`, `skills`,
  `compact`, `view`, `end`: `kelir ctl <session|latest> <verb> [args]`. They apply
  at named boundaries, never between a tool call and its result.
- **The prompt is frozen.** `AGENTS.md`, `CLAUDE.md` and the settings you started
  with are in the prefix; a change on disk arrives as a `source` message at the
  tail, never as a rewrite.
- **Config is two files**: `<config>/config.toml` (machine) then the nearest
  `.kelir/config.toml` (project, wins per key). `KELIR_*` variables cover paths
  and a few knobs; the two provider keys are the only secrets.
- **There are two providers** (`openrouter`, `opencode-go`) and the model's
  window, vision and effort support come from the provider's model list, not
  from a table in the source.

## Where the depth is

`kelir docs source-map` says which file owns what. The same files are in
`docs/reference/` when a checkout is present; the paths below are the ones a
change goes through.

- `src/config.rs`: `RawConfig`, `RawProvider`, `load` (every key).
- `src/envs.rs`: `VARS` (every environment variable; the only place one is read).
- `src/cli.rs`: the clap surface (`kelir docs --generate` writes the table).
- `src/verbs.rs`: `Verb`, `parse`, `Boundary`, `Scheduler`.
- `src/sources.rs`: prompt, instruction files, skill discovery, the watcher.
- `src/event.rs`, `src/fold.rs`: the log's shape and the request built from it.
- `src/provider.rs`, `src/retry.rs`, `src/models.rs`: requests, backoff, model metadata.

## Changing kelir itself

A change that adds a key, a flag, a verb, an event type or an environment
variable is not done until the reference says so: `kelir docs --generate`, then
`cargo test --lib docs` (the drift test reads the code both ways).
