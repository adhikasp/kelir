# Sources, skills and memory

**Summary:** what goes into a session's context besides the conversation — the
base prompt, `AGENTS.md`/`CLAUDE.md`, skills, hints, memory and attachments — how
each is found, how big it may be, and how an edit mid-session is announced.
**Read this when:** the model does not seem to know a rule you wrote down, a skill
is not being found, or you are changing discovery (`src/sources.rs`,
`src/attachments.rs`, `src/config.rs`, `src/prompt.md`).

## The base prompt

`src/prompt.md`, embedded (`DEFAULT_PROMPT`) and kept at **1 KB or under**
(`cargo test --lib sources` fails past it, and the bytes are pinned by
`tests/fixtures/prompt.md.snapshot` so a rewording is deliberate — it is part of
every request's cached prefix). `{{skills_dirs}}` is where the skills
directories are named; a prompt without the placeholder gets them appended, so a
hand-written one cannot lose the pull path.

Overridden, weakest first: the config's `prompt` file, `<config>/prompt.md`,
`<cwd>/.kelir/prompt.md`. `sources[]` records which one won (no path means the
embedded default — never the sentinel `embedded`).

## Instruction files

`AGENTS.md` and `CLAUDE.md`: the config dir's own, then every one found walking
up from the session cwd, nearest first. The walk stops at the repository root
(a directory holding `.git`) and at the home directory, so a file planted above
them is not picked up. Limits: 64 KB per file, 256 KB in total (past the total a
file is skipped with a note, not truncated — the watcher has to be able to
reproduce the read).

## Skills

Directories, searched read-only, machine layer first:

1. `~/.claude/skills` (the ecosystem's own),
2. `<config>/skills`,
3. each `skills_dirs` entry from the config (`~/` and `$HOME/` expand),
4. `<cwd>/.claude/skills`,
5. `<cwd>/.kelir/skills`.

A skill is a **directory** holding a `SKILL.md` with Agent Skills front matter
(`name`, `description`, optional `metadata`), at most 64 KB. Its name is the
directory's name; a project skill of that name replaces a machine one. A
directory whose `SKILL.md` is missing, unreadable or malformed is skipped with a
note (`error` event, `where: "source"`), never listed.

**Skills are pull.** The base prompt names the directories and nothing else: the
skill's body costs no token until a task needs it, and then it is `bash`
(`cat <dir>/<name>/SKILL.md`). `sources[]` still records each skill (name, real
path, hash), so editing one mid-session is noticed.

### The builtin `kelir` skill

`src/docs/skill-kelir.md` is embedded and listed like any other skill, whatever
the directories on disk hold: it describes how to configure and operate kelir
itself (config keys, CLI, verbs, providers, files) and points at
`kelir docs <topic>` and this reference's source map. It works in a session with
no checkout on disk, which is the point — a session that cannot find the repo can
still read how kelir works.

- **Layer:** machine, so a project skill called `kelir` replaces it.
- **No file:** `sources[]` records an empty path and `SourcePath::Builtin` is the
  typed absence of one, so there is nothing to watch, re-read or announce as
  changed. Editing the repo's copy takes a rebuild, and the drift test fails if
  the file stops being a valid Agent Skill.

## Hints, memory and channels

A `source` event is how *anything* that arrives after the start reaches the
model, as a message at the tail (never as a rewrite of the frozen prompt):

| `kind` | from |
|---|---|
| `hint` | a pushed hint (M5-B): a file or a line of steering, from `kelir ctl` or a gate |
| `skill` / `instruction` | the `skills` verb, or an edit to a file `sources[]` records |
| `memory` | a session's own notes (M9) |
| `attachment` | `kelir view`: one image, copied into the session's `attachments/` |
| `channel` | an inbound message from a bridge (M10) |

An **edit on disk** is noticed at every boundary by re-reading each recorded
source and comparing a content hash (there is no watcher: a file changes and the
next boundary says so). Editing a file *back* to an earlier body is announced
again; a deletion is one `error`-free note, not one per boundary. A source
carried into the context and then folded away by a compaction is re-announced at
the next boundary.

## Attachments

`kelir view <path>` is a pointer, not a picture: it sends the `view` verb over the
session socket, the session copies the file into `<session>/attachments/`, and
the next message carries it as one `image_url` part of the **last** message.
Everything earlier stays byte-identical, so the provider's cached prefix
survives. Refused (a message, exit 1, and nothing recorded): a missing file or a
directory, over 10 MB, an unknown type, bytes that are not an image (the
extension is only a claim — the signature is sniffed, and the `data:` URL's media
type is the sniffed one), and a model the provider marks text-only. The path is
resolved against the session dir first, so `attachments/1-x.png` is enough.
