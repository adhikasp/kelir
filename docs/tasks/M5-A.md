# M5-A — sources and skills (read AGENTS.md, CLAUDE.md and skills at session start)

Prereq: M4 green (HEAD `1dc2021`, all gates and the no-flash re-check passed). This is the *first*
slice of M5 in `docs/SPEC.md` (*Sources, skills and memory*, *The system prompt*): items 1, 2 and 3
of `docs/tasks/M5.md`. The pushed hint (4), memory (5), attachments (6) and `observe` (7) are M5-B,
a later job; do not build them here.

Why this slice first: today kelir starts blind — the model has to be told everything in the task
string. Once sources exist, every later session reads the project's `AGENTS.md` and the skills
directories by itself, which makes the harness better at every subsequent job.

## Build

1. **Sources**, two layers, machine then project, project wins on a name clash:
   - base prompt: an embedded default `prompt.md` of **at most 1 KB**, overridable by a file in the
     machine config dir and by one in the project;
   - instruction files: walk up from the session cwd and collect `AGENTS.md` and `CLAUDE.md`
     (nearest first, both layers, a project file over a machine file of the same name). Read-only.
   - skills directories: machine (`~/.claude/skills`, config `skills_dirs`) and project
     (`.claude/skills`, `.kelir/skills`), read-only. A skill is a directory holding `SKILL.md`
     with Agent Skills front matter (`name`, `description`, optional `metadata`).
   - Render the system **once** at `session_started`: `system` gains the rendered text plus its
     hash, and a new `sources[]` array carries `{name, path, hash}` for everything that went in. The
     date goes in, never the time; the same inputs must render byte-identical.
2. **Mid-session changes** never rewrite the system: a watcher or a per-turn mtime/hash check on the
   recorded sources (pick one, say why in `DEVIATIONS.md` if you deviate) appends a `source` event
   (`kind: instruction|skill`, `name`, `text` or `path`) which renders as a message at the tail.
   The `skills` verb (`kelir ctl <id> skills ...`) adds or replaces one skill or instruction source;
   it lands at the next boundary like every other verb.
3. **Skills are pull**: the base prompt names the skills directories and nothing else — no listing,
   no descriptions in the prompt. The model reads a `SKILL.md` with `bash` when the task calls for
   it. (The pushed hint that scores skills is M5-B.)

## Rules

Read `AGENTS.md` in the repo root first (`cat AGENTS.md`) — sources are not implemented yet, so you
have to read it like a human — then the spec sections named above and `docs/DEVIATIONS.md`.

- Work on `main`, small commits named `M5: <what>`, stage explicit paths (never `git add -A`).
- Windows is first class; no Unix-only assumptions without `cfg`; the shell for `bash` calls is Git
  for Windows.
- Public repo: no personal data, hostnames, usernames, keys or private paths in any file, fixture,
  log or test. Never copy the maintainer's home paths into a test.
- Footprint: binary ≤ 20 MB, idle RSS ≤ 30 MB, no SQLite, no daemon. Justify any new dependency in
  the commit message.
- No leaked processes and no visible console windows (the hard rule in `AGENTS.md`): kill anything a
  test or script starts, and start spawns with `CREATE_NO_WINDOW` on Windows.
- Start no other agents.

## Tests (the pyramid in `AGENTS.md`)

- **Unit**: discovery precedence (project over machine, nearest instruction file first, missing dirs
  tolerated), skill front-matter parsing including malformed and missing fields, render determinism
  (same inputs → identical bytes, hash stable), the 1 KB base-prompt budget, an mtime change →
  exactly one `source` event, the `skills` verb adding and replacing.
- **E2E (mock)**: `session_started` carries `sources[]` with the real files; the first request's
  system contains the instruction text; editing `AGENTS.md` mid-session appends a `source` message
  and leaves the system prefix byte-identical (assert the prefix, not just "no crash"); editing it
  back does not duplicate the source.
  The headline check: the request the mock receives for the second step has the first step's
  prefix unchanged (**prompt caching** is the point of rendering once).

## Gates (paste the counts in your report)

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, full `cargo test`, then the Linux
gate: `bash scripts/remote-check.sh <linux-machine>` — the orchestrator tells you the machine name
and it goes on the command line only; **no repo file, test or log may contain it**. Then check `tasklist` for `kelir.exe` / `bash.exe` your runs left behind.

## Live drive

One short pane run of the real binary against OpenRouter (`deepseek/deepseek-v4.1-flash-1017`):
a scratch project containing an `AGENTS.md` and one skill; ask a question that can only be answered
from the instruction file, and check `events.jsonl`'s `session_started.sources[]` and that the model
answered from the file. Env for live panes: `C:/Users/the maintainer/kelir-scratch/live.env` (source it inside
the pane command; never print or copy it).

## Done when

Gates green on Windows and Linux, a `PROGRESS.md` row appended for M5-A (date, commit range, test
counts, live verdict, known gaps), and this report:

`hc send t9m "M5-A sources+skills: <done|stuck>; <commit range + counts>; <what is left>"`
