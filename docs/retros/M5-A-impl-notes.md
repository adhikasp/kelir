# M5-A implementation notes — sources and skills

**Commit range:** `ccd556b..9826dbe` (three commits, each with its own tests green).
**Gates:** `cargo fmt --check` clean, `cargo clippy --all-targets -- -D warnings` clean,
`cargo test` green (counts below) on Windows and on the Linux box via
`scripts/remote-check.sh` (`FMT-OK`, clippy clean, every suite green).

| | Windows | Linux |
|---|---|---|
| unit (`--lib`) | 280 | 285 |
| `e2e` | 16 | 16 |
| `e2e_m3` | 11 | 13 |
| `e2e_m4` | 10 | 8 |
| **`e2e_m5` (new)** | **6** | **6** |
| `e2e_tui` | 15 | 15 |
| `e2e_verbs` | 23 | 23 |
| `review_m0` / `m1` / `m2` / `m3` | 8 / 5 / 2 / 11 | same |
| `review_m4` / `m4b` / `steer1` | 19 / 7 / 6 (2 ignored) | same |

The Linux totals differ from Windows only in the `cfg`-gated tests that already existed:
the five extra Unix unit tests (IPC/lock/kill-group), the two extra Unix `e2e_m3` cases
(stale socket, repeated race) and the two Windows-only `e2e_m4` console-less-parent cases.

## What was built

**One module, `src/sources.rs`**, holds discovery, the skill parser, the render and the
watcher. Nothing else knows how a source is found; `session.rs` only calls it.

- `Places` is every directory a session looks in, built once from the config
  (`skills_dirs`, the machine config dir) and the environment (`~/.claude`). Tests build
  a `Places` by hand, so no unit test touches a real home.
- `discover` returns machine sources then project ones, nearest instruction file first.
  A name clash is resolved in `push_source`: **the project wins over the machine, and
  within a layer the first (nearest) file wins.** That is the spec's sentence read
  literally, and it is what upstream harnesses do.
- `parse_skill` takes Agent Skills front matter (`name`, `description`, optional
  `metadata`, unknown keys tolerated, quoted values, a BOM, CRLF) and answers a reason
  for every malformed shape instead of guessing.
- `render_system` is pure apart from reading the files: the **date is a parameter**
  (`YYYY-MM-DD`, never the clock), skills are sorted by name, and the only iteration over
  a directory that can vary is sorted in `skills_in`. Same inputs → identical bytes is
  therefore a property of the signature, not of a test.
- `Watch` is the mid-session check: a `{kind, name, path, hashes[]}` list, where `hashes`
  is every body this context already carries (the rendered one and each `source`
  message). `changes()` re-reads each file at a boundary and answers only bodies that are
  new. That is what makes "one edit → one message, edit back → none" true without a
  watcher thread.

**The `skills` verb** carries `{kind, name, path?, text?}`. `--path` may name a file or a
skill directory (then `SKILL.md`); a skill's front-matter `name` must match the name it is
registered under, so a directory cannot be mislabelled. A name is `[A-Za-z0-9._-]{1,128}`
and may not start with `.`, so a verb can never point a source outside the session by
name. A body the context already holds byte for byte is refused ("already carries exactly
this text") rather than appended twice.

## Decisions worth flagging (all in `docs/DEVIATIONS.md`)

1. **The check is a per-boundary hash, not a watcher.** A watcher is a thread, an OS
   notification subscription or a handle — three things that can leak on a desktop that is
   already hosting someone's work. The recorded set is a handful of small files (a base
   prompt, at most a few instruction files, one file per skill), and re-reading them at a
   boundary costs a few hundred microseconds. The hash also makes the check exact: an
   `mtime` that changed with an unchanged body is not news, which `mtime` alone would
   report.
2. **Skills are recorded in `sources[]`, their bodies are not in the system prompt.** The
   spec's `session_started.sources[]` "carries `{name, path, hash}` for everything that
   went in", and a skill did go in — as a *pull* target. Recording it is what lets a skill
   edited mid-session arrive as a `source` message (the milestone asks for that), and the
   hash is of the file read from disk, front matter included.
3. **The base prompt's own edit arrives as `kind: "instruction"`.** The spec's `source`
   kinds have no `prompt`; the base prompt is identified by its name, `prompt.md`, and the
   embedded default records its path as `embedded`, so neither a log reader nor `observe`
   can confuse it with a project file.
4. **`KELIR_HOME` is new.** Discovery reads `~/.claude`, so a test run on a developer's
   machine would otherwise depend on that machine's skills. `paths::home` honours
   `KELIR_HOME` first, and the test harness points it at its own temp dir.

## Two harness bugs this work exposed (both real, both fixed here)

- **The suite inherited the running session's environment.** kelir dogfoods itself, so a
  `cargo test` run from inside a kelir session handed the child `KELIR_SESSION_DIR`,
  `KELIR_SOCK`, `KELIR_CALL_ID` and `KELIR_BIN`. The child then looked for jobs in *the
  session's* `bg/` directory: `e2e_m4`'s three process tests and `review_m4`'s R7 failed
  with the session's own job output in their assertion messages. They fail identically at
  clean HEAD with the variables removed from the environment, which is how the cause was
  isolated. The harness now scrubs the four variables on every spawn.
- **That same leak killed a run.** A test that inherited `KELIR_SOCK` reported a job into
  the session driving the suite, and a test that inherited `KELIR_SESSION_DIR` cancelled
  a job in it. One `cargo test` here died mid-run for exactly that reason; it is worth
  remembering that "kelir dogfoods kelir" makes environment hygiene a safety property, not
  tidiness. The two places that build a command by hand (the `bg out --follow` timing
  test, and the `bg cancel` cleanup in `e2e_m4`) now scrub the variables themselves.

## What is not here (M5-B)

The pushed hint (a scoring call per user prompt), memory, attachments and `observe` are
M5-B by the brief. `sources.rs` leaves the seams they need: `Rendered.sources` is what
`observe` prints, `Watch::note_text` accepts a body that arrived from anywhere (a hint, a
memory note), and the `skills` verb's `kind` is an enum so a new kind is one arm.

## Known gaps

- A skill's `description` is parsed and kept but is not read by anything yet: it belongs to
  the pushed hint (M5-B).
- A source that is *deleted* mid-session is not reported (only an edit is). Removing an
  instruction file is rare and the message would have nothing to carry; if M5-B's memory
  directory wants deletions, `Watch::changes` is the one place to add them.
- The system prompt names every skills directory whether or not it exists. That is
  deliberate (a directory can appear mid-session and the model should know to look), but it
  means the prompt of a project with no skills still lists three paths.
