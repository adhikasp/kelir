# kelir

A small, steerable coding-agent harness. `kelir` runs a coding model against a
workspace, streams what it does to an append-only event log, and exposes the
session to other tools through a CLI, a TUI and a set of named verbs. It is
built in Rust; the release binary is small (target ≤ 20 MB, idle RSS ≤ 30 MB),
needs no daemon and no database, and runs on Windows, Linux and macOS.

## Why

Most agent harnesses are opaque about what the model did and hard to drive from
another program. kelir is the opposite: every turn is an event in
`events.jsonl`, every subcommand is scriptable, and the same code path serves
interactive use and automation. It was written as the harness that builds
itself, so dogfooding and reproducibility are first-class concerns.

## Install

From crates.io (once published):

```sh
cargo install kelir --locked
```

From source (Rust 1.88 or newer):

```sh
git clone https://github.com/<owner>/kelir.git
cd kelir
cargo build --release
./target/release/kelir --help
```

Run `kelir doctor` to check the toolchain, the shell and credential sources.

## Configure

Configuration merges a machine file and a project file, with the project
winning per key:

- machine: `%APPDATA%\kelir\config.toml` on Windows,
  `$XDG_CONFIG_HOME/kelir/config.toml` or `~/.config/kelir/config.toml` on
  Linux, `~/Library/Application Support/kelir/config.toml` on macOS;
- project: the nearest `.kelir/config.toml` walking up from the working
  directory;
- `KELIR_CONFIG` or `--config` selects one file and ignores the other two.

A minimal config:

```toml
provider = "openrouter"
model = "deepseek/deepseek-v4.1-flash-1017"
```

A literal `key` in config is refused: keys never live in files. All keys are
listed in `docs/reference/config.md` and `docs/reference/env.md`.

## The two providers

kelir supports exactly two providers; any other name is a usage error.

- **`openrouter`** — reads `OPENROUTER_API_KEY`, an alternate variable named by
  `key_env`, or the trimmed stdout of a command named by `key_cmd`. Use it with
  any model id served by OpenRouter.
- **`opencode-go`** — uses `KELIR_GO_API_KEY` as an explicit override when set,
  otherwise reads `opencode auth export opencode` directly, so a token does not
  have to be copied into a variable that goes stale. The store read is bounded
  at 5 s and re-read only near expiry.

Neither provider is hardcoded to a model. Context window, vision and effort
support come from each provider's `/models` response. Retries use exponential
backoff with jitter and respect the provider's own reset hints; exhausting the
budget pauses the turn rather than losing it. See `docs/reference/providers.md`.

## Using it

```sh
kelir                       # interactive TUI
kelir run "fix the flaky test"   # one task, events on stdout with --json
kelir ask "what does this do?"   # one question
kelir resume <SESSION>      # continue a session
kelir doctor                # diagnose the setup
kelir docs                  # the built-in reference
```

`kelir ctl <verb>` exposes session operations (fold, edit, attach, notify,
etc.) for scripts; `kelir job`, `kelir bg` and `kelir run --child` cover named
sessions and supervised children. The full verb and CLI tables are generated
into `docs/reference/`.

## Safety and autonomy

**kelir is a fully autonomous agent: it does not ask for approval and there is
no human in the loop.** It runs shell commands, edits files and starts child
processes with the full privileges of the user who started it. Treat it like
any other powerful local tool:

- run it on a workspace you trust, with an account that has only the
  permissions the task needs;
- keep credentials in environment variables or a credential command, never in
  config files, logs or fixtures;
- use the owner-only state directory and don't share a session token: the
  token grants control of that session;
- if you need a policy layer, use the event **gates** (`docs/reference/gates.md`)
  to allow, deny or rewrite tool calls at named events — gates are the intended
  place for guardrails, not interactive prompts;
- be aware that child processes and background jobs can outlive a single
  tool call; the harness kills the trees it starts, but reviewing `kelir ls`
  after an unusual run is good practice.

kelir records key *sources* (never values) in the event log, scrubs `KELIR_*`
variables before spawning children, and creates session directories and tokens
with owner-only permissions. See `docs/SPEC.md` for the trust model and
`SECURITY.md` for how to report a problem.

## Documentation

`docs/SPEC.md` is the authoritative specification; `docs/reference/` is the
generated and hand-written manual; `docs/PROGRESS.md` records what has landed;
`docs/PUBLISH.md` records the publication procedure. Run `kelir docs` for the
built-in reference.

## License

The license is pending the maintainer's choice between MIT and Apache-2.0.
Both texts are present as `LICENSE-MIT` and `LICENSE-APACHE`; exactly one will
be selected before publication. See `docs/PUBLISH.md`.
