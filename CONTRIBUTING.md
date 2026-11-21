# Contributing

## Ground rules

- This is a public repository: never commit personal data, hostnames,
  usernames, API keys, private paths or secrets — not in code, tests, fixtures,
  logs or docs. Credentials come only from the environment
  (`OPENROUTER_API_KEY`, `KELIR_GO_API_KEY`).
- Windows is a first-class target. Never hardcode Unix paths or Unix-only APIs
  without a `cfg` and a Windows branch, and never hardcode Windows paths in
  tests that run everywhere.
- Footprint is a feature: keep the release binary ≤ 20 MB and idle RSS ≤ 30 MB.
  Add a dependency only when the spec names it or the commit message justifies
  it.
- No leaked processes and no visible console windows. Any test that spawns
  processes must cap them, kill the whole tree on drop or failure, and assert
  that nothing survives. On Windows, start children with `CREATE_NO_WINDOW`.
  Never send console signals to a shared process group.
- Spec first: `docs/SPEC.md` is authoritative. Write any deviation in
  `docs/DEVIATIONS.md` with a reason.

## Worktrees, one writer per worktree

Work happens on a job branch in its own git worktree, never in another job's
tree. Each job gets its own branch (`job/<name>`), target directory, state
directory and Linux verification work dir:

```sh
git worktree add -b job/<name> ../<name> main
cd ../<name>
```

Only the integrator merges branches, one at a time, after that branch passes
its gates and gets a fresh reviewer's verdict. Keep changes localized: several
jobs may touch the same shared modules, so prefer new modules and surgical
edits. Commit in small steps with `Mn: <what>` messages and explicit paths;
never force-push or rewrite shared history.

## Gates

The pyramid is: unit (`cargo test --lib`), integration/e2e against the
scripted mock server (`cargo test --test '*'`), and a live drive of the release
binary against a real model (reviewer, not CI).

```sh
cargo test --lib <module>          # the fast loop
bash scripts/gate.sh --quick       # fmt + clippy before a commit
bash scripts/gate.sh               # the full gate on this machine
bash scripts/gate.sh --linux=<machine>   # include the Linux half
```

`gate.sh` takes the one machine-wide lock, caps jobs and test threads, and
runs fmt, clippy `-D warnings`, the full nextest suite (with retries) and the
doc tests. The machine name for the Linux half comes only from the flag or
`KELIR_LINUX`, never from a file in the repo. Do not run stages by hand in
parallel with someone else's gate — the lock is what keeps workers from
flaking each other. Core dumps are disabled because crash-injection tests
abort on purpose.

## Docs are part of done

Any change to a config key, CLI flag, env var, verb, event, file location or
TUI key must update `docs/reference/` in the same series of commits. The tables
there are hand-maintained markdown: edit the row in the matching topic (`config.md`,
`verbs.md`, `events.md`, `env.md`); a new env var also goes in `src/envs.rs`,
which is the runtime accessor. Then run:

```sh
cargo test --lib docs
```

The drift test reads the code and fails if a name exists there but is missing
from the reference. `kelir docs --search <words>` finds a topic from inside a
session with no checkout.

## Scrub check

Before reporting, run the scrub check with the pattern file the orchestrator
provides (the pattern file lives outside the repository):

```sh
bash scripts/scrub-check.sh <pattern-file>
```

For publication, the scrub must also cover all history; see `docs/PUBLISH.md`.
