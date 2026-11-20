
# kelir — spec

**Status:** spec. Implementation tracks the milestones at the bottom.

## Frame

A hobby harness whose point is **steerability** and **opinionated defaults**, run as a **fully
autonomous agent**: no approvals, no asking permission, no policing of tool calls. Feature parity
with opencode is not a goal. Opinions live in skills and gates (data and scripts), never in the
core.

## Architecture

**One session = one OS process + one append-only event log.** Every harness capability is a verb
that appends an event; the loop applies verbs at named boundaries.

```
<state>/sessions/<id>/
  events.jsonl            # append-only, fsync'd per batch, monotonic seq; the only source of truth
  commands/<call-id>/out  # full tool output, never truncated; also the >10 KB spill file
  bg/<bg-id>.status       # written by the bg shim; survives kelir exiting; the dir is 0700
  sock                    # the session socket (a named pipe on Windows)
  token                   # this session's secret, owner-only (0600), removed when the session ends;
                          # `kelir ctl` reads it while it lives
  bg/<bg-id>.cancel       # exists only while a `cancel` is in flight (so a reader says
                          # "running", not "lost", between the kill and the status write)
```

- **Session id:** `ses_<ulid>`. It is also the provider cache/session key (see *Prompt caching*).
- **State dir:** `~/.local/state/kelir` (Linux, honours `$XDG_STATE_HOME`),
  `~/Library/Application Support/kelir` (macOS), `%LOCALAPPDATA%\kelir` (Windows). Config:
  `~/.config/kelir` (Linux, macOS), `%APPDATA%\kelir` (Windows), then `.kelir/` in the project.

### Process model

- The **session process** is the foreground `kelir` (the TUI, or `kelir run` headless). It is the
  only writer of `events.jsonl`, serves the session socket, holds the MCP connections and owns
  every tool child. There is no daemon.
- The **TUI runs inside the session process.** Other terminals reach a session through the socket
  (`kelir ctl` for verbs, `kelir attach` to watch); a client disconnecting never affects the
  session.
- **Exiting kelir stops the session:** running tool calls are killed with their process trees,
  as `stop`. Background jobs are the exception (they are detached on purpose, below). Picking a
  session up again is `kelir resume <id>`.
- **A bg job that finishes while its session is not running** is only recorded: its shim writes
  `bg/<id>.status`, and the next `resume` folds it into the log as `bg_settled`. Nothing wakes a
  stopped session.
- **Children are separate OS processes** the model starts from `bash` (`kelir run --child`,
  `kelir brief`; there is no spawn tool), each owning its own session dir. They live inside the parent's process group (Unix) or Job Object (Windows), so
  killing the parent leaves no orphans.

### Who may drive a session

Reaching the socket is **not** authorisation. Two things gate every request:

- **A per-session token.** At start the session makes 32 random bytes (hex), keeps them in
  memory, writes them to `<session dir>/token` with owner-only permissions (`0600` on Unix; on
  Windows a protected DACL with one ACE for this user) and exports them to its own tool children
  as `KELIR_TOKEN`. Every request that arrives on the socket must carry the token; the listener
  answers and closes a request that does not, **before** the session sees it, so an unauthenticated
  caller cannot even write a `verb` event. The token never appears in a log line, an event or an
  argv (a bg shim gets it in its environment, not on its command line). `kelir ctl` from a human
  terminal reads `<session dir>/token` itself, so it keeps working; a bare socket client reads
  `$KELIR_TOKEN` from the environment it inherited. **A process that can read the session's
  environment or its token file already runs as the user, in the session, and is trusted** — the
  token stops a *different* session's child, a stray process and a script that found the pipe by
  name, not the user or the model's own tools. The token file is removed when the session ends: a
  token nobody serves is only something to find.
- **A registry of the jobs this session created, a capability per job, and a signing key per
  job.** The session is the only issuer of ids for explicit jobs (`bg_<32 random hex>`, minted on
  `bg-claim`/`bg-spawn`); a bash call's `bg/` id is the deterministic `<seq>-<step>-<n>` its
  output directory is named after, so two sessions can share those names (each has its own `bg/`,
  and every id is checked against *this* session's registry and keys). An id is recorded in an
  in-memory registry *and* in the log (`bg_started`, and `tool_started.dir` for a call), and
  rebuilt from the log on resume. `bg` reports, `cancel` and `bg-spawn` name an id, and the
  session answers only for one in the registry. A `bg` report must *also* carry **that job's
  capability** (32 random bytes the session generates and hands only to that job's shim), because
  every tool child holds the session token by design. A request that names an unknown id, an id that is not
  a job (a call's own id, a job adopted by a resume) or the wrong capability is refused with a
  clear error and a `verb` event with boundary `rejected`, rate-limited to `32` such events per
  session. **No secret ever leaves the session or the job's own shim:** neither the capability nor
  the signing key is returned by any verb. `kelir bg run` sends the internal `bg-spawn` verb with
  its command, working directory and environment, and the **session** mints the id, starts the
  shim and hands it the key and capability on the shim's stdin pipe; the answer is the id and
  nothing else. From a `bg-spawn` request the session honours only `cmd`, `shell`, `cwd` and
  `env` (filtered: well-formed names, bounded, and without `KELIR_TOKEN`/`KELIR_SOCK`); the shim's
  kind, the socket it reports to, the token and the Windows job name are the session's own, and
  a `call` is kept only if that call is running. No verb writes its request wholesale to the log:
  each verb logs a fixed set of fields, and a `bg-spawn` logs only its `dir`, `cwd` and the
  number of environment variables (never the environment, the token or the command). Tool
  children (every `bash` call and job) do not inherit `OPENROUTER_API_KEY` or `KELIR_GO_API_KEY`;
  a command that needs a provider key must be given one explicitly. A key under a custom
  `key_env` name is the user's own variable and is passed through. A `bg-spawn` that names a job id (owned or not) is refused, and `bg-claim` (retired) is an
  acknowledgement (an id) only, from a budget of `16` of its own. No verb reads or echoes a caller-named path: a settled job's `out`
  must lie inside the session's own `bg/`. The number of ids one session mints on request is
  bounded (`256`). Every job directory and
  status/out path in a request must resolve **inside the session's own `bg/` or `commands/`**,
  canonicalised: `..`, a symlink out of it and, on Windows, a UNC or another drive's path are all
  refused.
- **A status file is evidence only if the job's shim signed it.** Writing `bg/<id>.status` is
  within every tool child's rights, so the file's existence or contents prove nothing. The
  session mints an Ed25519 key pair per job (per bash call too); the private key goes to the
  session's memory and to that job's shim **over an inherited pipe** (the only copy outside the
  session, and never returned to a caller) (not argv, not the
  environment, which a same-user process reads through `/proc/<pid>/environ` or the PEB, not a
  file), and the shim does not pass it to the command it runs. The shim signs every status record
  (`sig`, hex, over the record without it); the session verifies every record it folds into the
  log (the watcher, the resume pass, a `bg` report, a promotion) under the job's public key,
  which `bg_started` records (`pubkey`) so a resume can verify a shim that outlived its session.
  An unsigned or badly signed record is ignored and recorded once per id as a refused `verb`
  event. After a resume, a job whose public key the log does not hold cannot be authenticated and
  is reported `lost`. Every signed record carries a **sequence number** (covered by the signature,
  with the job id) that only goes up; the session refuses a record older than one it has believed,
  and a settled job's final record is also kept in `bg/<id>.final`, which wins over the status
  file, so putting an older `running` record back cannot un-settle a job (a replay can withhold a
  result, never change one).
  **What this does not cover.** `kelir bg ls|out|wait|cancel` run in a tool child or a terminal,
  hold no key, and read the files as a view; they never write the log. A process of the same user
  that can read another process's memory can read a shim's key. A job's shim narrows this as far
  as the platform lets an unprivileged process: on Windows it removes `PROCESS_VM_READ` and
  friends (and `WRITE_DAC`) for its own user with an Owner Rights DACL, and on Linux it clears
  `PR_SET_DUMPABLE`; it holds the key only for its lifetime and overwrites its own copy when it
  exits. That stops a sibling tool child's ordinary `ReadProcessMemory`/`/proc/<pid>/mem` read. It
  does **not** stop an elevated administrator enabling `SeDebugPrivilege`, root or a debugger run
  by the same user as the shim's parent, so those stay outside the boundary. The **session
  process** holds every job's key, so it gets the same treatment at startup, before the first key
  exists: on Windows the Owner Rights DACL (terminate, set-quota, query-information and synchronize
  remain, so `taskkill`, the harness and the liveness checks still work; memory read/write,
  thread creation, handle duplication and `WRITE_DAC` do not); on Linux
  `PR_SET_DUMPABLE=0`. On Linux that is what protects the keys against a same-user process where
  `kernel.yama.ptrace_scope=0`; with Yama at 1 or more the kernel already refuses it. Every shim
  that holds a key is hardened, whatever its `--kind`. And `events.jsonl` and the session directory
  are writable by any same-user process, so code that deliberately rewrites the log (a forged
  `bg_started.pubkey` or `tool_started.dir`) is inside the boundary. The boundary is other users,
  other sessions, stray processes and **the model's own tools acting through the files and the
  socket the design exposes**; it is not same-user code that attacks the session's memory or
  rewrites its log on purpose. A signature cannot stop a child from deleting or rolling back a
  status file (the shim's next write restores it; a job that never writes again reads as `lost`).
- **The socket and the pipe are owner-only too.** The Unix socket is `0600` in a directory only
  the user can enter; the Windows pipe's DACL grants this user and no one else. Both are read
  back at start: a session that cannot prove its socket is private says so and refuses to serve.

## What the model gets

### Native tools (the whole tool list; fixed for the life of a session)

| tool | does |
|---|---|
| `bash` | runs a command in the configured shell (see *Shell and platforms*); output tee'd to `commands/<id>/out` |
| `edit` | string replace in one file: `{path, old, new, replace_all?}`; `old` must match exactly once unless `replace_all`; `old: ""` on a missing file creates it. Preserves the file's line endings (CRLF stays CRLF). Returns the changed line numbers. Reading files is `bash` (`cat`, `sed -n`, `rg`). |
| `compact` | compacts this session's context (see *Compaction*); takes an optional `focus` |

Everything else is a CLI the model reaches through `bash`, so it costs no tool schema:
`kelir mcp …` (MCP servers), `kelir bg …` (background jobs), `kelir ls` (sessions and children),
`kelir doctor` (which provider is wired to which key source — never the key; see *Providers*).

**Parallel tool calls are allowed.** All calls in one assistant message run concurrently; their
results are appended in **call order, not completion order**, so the log and the next request are
deterministic.

### Verbs (what a human, a script or a gate can do to a session)

Every verb works the same from the TUI, `kelir ctl <session> <verb> [json]` and the socket, and
every one of them carries the session's token (*Who may drive a session*, above).

| verb | what it does | applies |
|---|---|---|
| `say` | inject a user message; `--now` = stop the running calls first | next boundary / now |
| `later` | queue a message for the end of the turn | turn end |
| `stop` | stop the turn: signal the tool process trees, keep partial work, `[Skipped]` results for unrun calls. `stop --call <id>` stops one running call; it returns `stopped by user: <reason>` and the turn continues | now |
| `cancel` | `<id>`: kill one of this session's background jobs with its process tree and record it settled `cancelled`; applies at once, even in an idle `--keep` session. A job id this session did not create is refused (a `verb` event with boundary `rejected`) | now |
| `resume` | continue a stopped, paused or crashed session from its log | now |
| `model` | swap provider/model for the rest of the session | next step |
| `tools` | disable/enable a native tool or an MCP server; the tool list sent to the provider never changes, a disabled tool's call returns `disabled by user` | next step |
| `skills` | add/replace a skill or an instruction source; arrives as a message | next boundary |
| `compact` | same as the tool; `/compact [focus]` in the TUI | next boundary |
| `brief` | side question as a child on a by-value snapshot; the session's history is untouched | async |
| `observe` | print exactly what the model will see next: system, tools, messages, token count, sources, hint, cache hit ratio | read-only |
| `recall` | ranked search over past sessions via a derived FTS5 index; built only when `rg` stops being enough ( §C) | read-only, later |

There is no approval verb and no `ask`: kelir never waits on a human. There is no `rewind`
(files are git's business, and a context rewind was judged not worth having).

## Event log v0

One JSON object per line: `{"seq": n, "ts": "<RFC 3339>", "type": "<type>", …}`. `seq` is
monotonic from 1. A torn last line is detected, skipped and reported on resume. Payloads over 10 KB
are never inline: the event holds the path.

| type | key fields | notes |
|---|---|---|
| `session_started` | `id`, `parent?`, `depth`, `cwd`, `os`, `kelir_version`, `provider`, `model`, `system` (rendered text + hash), `sources[]` (name, path, hash), `tools[]`, `reasoning_effort?`, `key_origin?` | the frozen request prefix is recorded here. `key_origin` names the key's *source* (`env OPENROUTER_API_KEY`, `key_cmd getkey`, `opencode auth export opencode`), never the key |
| `user` | `text`, `from` (`tui`, `ctl`, `channel`, `job`, `gate`), `delivery` (`now`, `next`, `later`), `attachments[]` | a human message; also what compaction pins |
| `source` | `kind` (`hint`, `skill`, `instruction`, `memory`, `attachment`, `channel`), `name`, `text` or `path` | context that arrived after the start; rendered as a message at the tail |
| `step_started` | `step`, `provider`, `model`, `request_hash` | one per provider request |
| `assistant` | `step`, `text`, `reasoning?`, `tool_calls[]` (`id`, `name`, `args`), `finish_reason` | |
| `usage` | `step`, `prompt_tokens`, `completion_tokens`, `cached_tokens`, `cache_write_tokens`, `cost`, `allowance?` | from the final stream chunk |
| `tool_started` | `call_id`, `name`, `input`, `pid?`, `dir?` | intent; written before the process starts. `dir` is the call's own `commands/<dir>` name (its job id), so a resume adopts it from the log rather than from a directory |
| `tool_settled` | `call_id`, `outcome` (`ok`, `error`, `stopped`, `skipped`, `denied`, `disabled`, `promoted`, `unknown`), `exit_code?`, `bytes`, `lines`, `out_path`, `spilled`, `result` | commit; `result` is what the model sees (≤10 KB or the spill notice) |
| `mcp_started` / `mcp_settled` | `mcp_id`, `server`, `tool`, `args`, `outcome`, `out_path`, `parent_call_id` | a `kelir mcp` call made from inside a bash call |
| `bg_started` / `bg_settled` | `bg_id`, `call_id`, `cmd`, `pid`, `exit_code?`, `out_path` | `promoted` tool calls and explicit `kelir bg` jobs |
| `child_started` / `child_settled` | `child_id`, `depth`, `task`, `background`, `outcome`, `result` | the child's own log is in its own dir |
| `verb` | `name`, `args`, `from`, `boundary`, `error?` | every verb received, applied or not; `boundary: "rejected"` is a refusal (a foreign job id, a path outside the session's own `bg/`, an unparsable verb), with `error` saying why |
| `gate` | `gate`, `event`, `decision`, `reason`, `ms`, `rewritten?`, `error?` | every gate run, including fail-open ones |
| `compacted` | `through_seq`, `summary`, `pinned_seqs[]`, `by` (`user`, `agent`, `auto`), `tokens_before`, `tokens_after` | the fold replaces everything up to `through_seq` except the pins |
| `turn_ended` | `outcome` (`done`, `stopped`, `paused`, `error`), `final_text`, `reason?` | `paused` covers two cases, distinguished by `reason`: `steps` (an explicit `max_steps` cap) and `provider` (the retry budget was exhausted). Neither is a failure: `--keep` stays alive and the next message opens a new turn |
| `error` | `where`, `message`, `retryable`, `status?`, `attempt?`, `retry_in_ms?` | provider errors, broken gates, torn lines. A retryable provider error's `message` is the status and its class (`http 429: rate limited`), never a response body |
| `resumed` | `from_seq`, `unsettled[]` | `unsettled` lists started-but-unsettled calls |
| `session_ended` | `reason` | |

**Context is always `fold(events.jsonl)`**, the same code path live and on resume. The fold
renders: the frozen system message from `session_started`; then, in `seq` order, `user`,
`assistant`, `tool_settled.result` and `source` events as messages; `compacted` replaces its range.

## Loop and boundaries

Per step: `fold` → request → stream → log → run tool calls (in parallel) → apply boundaries.
Boundaries are an enum (`before_step`, `after_tools`, `after_step`) applied in one `select!` arm,
and **never between a tool call and its result**: a `say --now` mid-batch stops the running calls,
gives each a `stopped`/`skipped` result, and only then injects the message.

**No step limit by default.** `max_steps` in `config.toml` (and `--max-steps`) caps the steps of one turn;
unset or `0` means unlimited, because a fully autonomous agent is not stopped by a clock or a counter.
The knob exists for jobs that want a cap. When an explicit cap is reached the turn is *paused*, not failed:
the log records `turn_ended` with outcome `paused`, `--json`'s `result` says `paused` with exit code 0, and
the message on stderr and in the TUI says "turn paused after N steps; say `continue` to resume"; the next
`say` starts a new turn with a fresh count (the session-wide step numbers keep counting) and `resume` of a
paused log picks it up like a stopped one. The TUI status line shows `step 12/20` only when a cap is set
(also while idle, before the first turn, and a paused turn reports Herdr `blocked`).

## Prompt caching (must work)

The request is built so its prefix only ever grows:

1. **System** = base `prompt.md` + machine and project sources, rendered once at
   `session_started` and frozen (the date goes in, never the time). A source file edited on disk
   mid-session arrives as a `source` message, not as a system rewrite.
2. **Tools** = the three native tools (`bash`, `edit`, `compact`), the same list for the whole
   session (and for `brief`, which refuses `edit`/`compact` by result rather than by a different list).
3. **Messages** = append-only. Hints, `skills` changes, memory, attachments, bg and child
   completions, channel envelopes and gate notes are new messages at the tail, never edits to
   earlier ones. Parallel results go in call order.

Only two things restart the cache, both expected and logged: `model` (another model has another
cache) and `compact` (it rewrites the prefix by design).

Per provider (OpenRouter's caching page read 2026-11-18):

- **OpenCode Go:** `x-opencode-session: <session id>` on every request (also required by Go's
  terms).
- **OpenRouter:** the session id as the body's `session_id`, which makes provider routing sticky
  from the first request (without it, only after a cache hit; stickiness lapses after 60 min idle).
  Never set `provider.order` (it overrides stickiness). DeepSeek, OpenAI, Grok, Moonshot, Z.AI and
  Gemini 2.5+ cache automatically; for Anthropic (and Qwen) models kelir sends the top-level
  `cache_control: {"type": "ephemeral"}`, which moves the breakpoint forward as the conversation
  grows, with `ttl: "1h"` configurable.
- **Children** get their own session id and cache; `brief` reuses its parent's id, so its
  snapshot prefix hits the parent's cache.
- **Measured, not assumed:** every `usage` event records `cached_tokens` and `cache_write_tokens`
  (`usage.prompt_tokens_details`); `observe` and `kelir ls` show the hit ratio. Anthropic does not
  cache under 512–4,096 tokens (by model), so the first steps of a tiny session missing is
  expected.

## Durability

- fsync per batch; a crash loses at most the in-flight step.
- On resume, a `tool_started` with no `tool_settled` is reported to the model as **unknown
  outcome and is not re-run** (naive replay re-runs `rm -rf`). The model decides what to check.
- No workspace snapshots: recovering files is git.

## Shell and platforms

**Linux, macOS and Windows are all first-class**, with CI on all three from M0.

| | Linux / macOS | Windows |
|---|---|---|
| shell (`shell` in config) | `bash -c` (falls back to `$SHELL`); a command over 6 KB runs from a file | Git for Windows `bash.exe`; `agshell` or `pwsh` by config. Every command runs from a file (switch kept: `/C`, `-File`) — see DEVIATIONS |
| process tree | own process group (`setsid`), `killpg` on stop | Job Object per call (kill-on-close), `CREATE_NEW_PROCESS_GROUP` |
| session socket | Unix socket | named pipe (both via the `interprocess` crate) |
| socket permissions | `0600`, re-read at start | owner-only DACL, re-read from the handle at start |
| detached bg job | `setsid`, outside the session's group | `DETACHED_PROCESS`, outside the session's Job Object |

No shell reimplementation. **A call running past 10 minutes is promoted to the background, not
killed** (outcome `promoted`; work in flight is never destroyed by a clock). A later optional
`supervised` mode can use the agshell brush-core trap trick to see inside loops and subshells.

## Big outputs

Any tool result over **10 KB** reaches the model as a notice instead: byte and line counts, the
path, a 1 KB head and 1 KB tail, and a one-line `rg`/`jq`/`sed -n` hint. The file is
`commands/<id>/out` in the session dir, never `/tmp`. **Sweep:** a session's `commands/` is
deleted 7 days after the session's **last event**, so a thread that is still in use keeps its
files. The log itself is kept.

## MCP as a CLI

```
kelir mcp ls [--json]                            servers: name, state, tool count, one line
kelir mcp <server> ls [--detail name|desc|full]  tools; full = inputSchema
kelir mcp <server> <tool> [k=v …] [--json '{…}'] call; --schema prints the inputSchema instead
```

Built on `rmcp` (client features only). stdout is `structuredContent` as JSON or the text blocks
joined; images and blobs go to files and their paths are printed; `--envelope` prints the whole
result. Exit codes: 0 ok, 1 tool `isError`, 2 usage, 3 server unavailable, 4 timeout (unknown
outcome, never retried). The session process is the broker: it starts a server lazily, keeps it
for the session and serves the CLI over `KELIR_SOCK` (exported to every bash child); outside a
session, `kelir mcp` spawns the server one-shot. Each call is an `mcp_started`/`mcp_settled`
pair, and gates see it as tool `mcp:<server>.<tool>`. Channel servers (`tg-bridge`) are connected
from the start because inbound messages arrive on that connection. Servers are configured in
`mcp.toml`, each with an optional `description` for the hint.

## Sources, skills and memory

- **Sources** come from two layers, machine then project (project wins on a name clash): the
  base `prompt.md` (≤1 KB), instruction files found walking up from the cwd (including
  `AGENTS.md`/`CLAUDE.md`), and skills directories (including `~/.claude/skills` and
  `.claude/skills`, read-only).
- **Skills** use the [Agent Skills](https://agentskills.io/specification) format; the
  `description` is the "when". **Pull:** the model reads a skill with bash; the base prompt names
  the skills directories and `kelir mcp ls`, and carries no listing. **Pushed hint:** one
  Decisions API call per user prompt scores skills and MCP
  servers; kelir appends "might be relevant: name — description" (never a body) as a `source`
  message. It fails open on a missing key or a timeout.
- **Herdr awareness (source, M5)**: herdr-core sets `HC_TERMINAL_ID`, `HC_TERMINAL_SOCKET` and `HC_SCOPE` in every
  pane it creates (`TERM_PROGRAM` is `ghostty` there and unusable). If `HC_TERMINAL_ID` is set, kelir renders
  **one line** at `session_start` into the frozen system sources: the pane's address, space, tab and `where` label,
  read once from `hc get $HC_TERMINAL_ID --json` (labels `herdr.space`/`herdr.tab`/`where`). It **fails open**:
  `hc` missing, slow (short timeout) or erroring just omits the line. **No live count is ever frozen into the
  prompt** (how many agents share the space is asked live, see the M8 `herdr` skill). Outside Herdr the line is
  absent. No new tool.
- **Memory** is a source: the project's memory directory is loaded at session start; the model
  writes it with `edit`; changes made mid-session arrive as `source` messages.
- **Attachments** (images, voice, video) enter as `source` events: voice transcribed at ingress,
  images to the prompt, video as frames.

## Self-documentation

Everything user-configurable is documented in `docs/reference/` (config keys with defaults, env overrides, flags, verbs,
files, events, providers, troubleshooting) with pointers into the source. What a machine can know for certain is
**generated**: `kelir docs --generate` rewrites the blocks between `<!-- generated:kind -->` markers (the CLI from clap,
the verbs/config keys/event types from `src/docs/tables.rs`, the environment from `src/envs.rs`, which is the only place
a variable is declared), and `kelir docs --check` fails when one is stale. The prose around the markers is hand-written.
A **drift test** is stricter than `--check`: it reads the source as well as the files, so a verb, key, event type or
variable that exists in one and not the other fails in **both** directions.

The same text is embedded in the binary (`include_str!`) and readable by the agent, which is how a session with no
checkout on disk learns to operate itself: `kelir docs [topic]`, `kelir docs --list`, `kelir docs --find <words>`.
A builtin skill `kelir` ("operate or configure kelir itself", `src/docs/skill-kelir.md`, no file on disk) is discovered
like any other skill, so the pushed hint and steering rules can inject it. The base prompt carries one line that names
`kelir docs`. Every milestone's definition of done includes updating `docs/reference/`; the drift test is what enforces it.

## Compaction

Three triggers, one mechanism: the model calls the `compact` tool, a human sends `/compact
[focus]` (or `kelir ctl … compact`), or the context passes 80 % of the model's window (from
the provider's model metadata; configurable). It applies at the next boundary after the current
batch settles. A summarising request (prompt in `compact.md`, configurable data) produces a
summary; the `compacted` event then replaces the range with it, **pinning verbatim** the first
user message (the task) and every message a human typed. Spill files are untouched, so the
summary can cite their paths.

## Gates: where every opinion lives

A gate is an executable kelir runs at a named event, which can **allow, deny or rewrite**. It is
the only way a veto or a nudge enters the core. Declared three equivalent ways: under `[gates]`
in `config.toml`, as a skill's `scripts/` file bound with `metadata: {kelir-gate: <event>}`, or
as an inline command string.

| event | `deny` means | `rewrite` replaces |
|---|---|---|
| `session_start` | nothing (side effects only, e.g. `git pull`) | — |
| `prompt_submit` | the prompt is dropped with a reason | the prompt text |
| `before_tool` | the call returns `denied by gate <name>: <reason>` | the call's input |
| `after_tool` | — | the result (or appends a note to it) |
| `turn_end` | the turn continues once with the reason as a message; **blocks at most once per turn** | the final text |
| `session_end` | nothing (side effects only, e.g. commit and push) | — |

The event goes to the gate as JSON on stdin (with the session dir and log path); the gate answers
`{"decision":"allow|deny|rewrite","reason":"…","input":{…}}` on stdout. **Timeout 30 s.** A gate
that crashes, times out or prints garbage is logged and treated as `allow` (fail open). Every run
is a `gate` event, so `observe` shows what fired.

As built (M8, details in `docs/reference/gates.md` and `docs/DEVIATIONS.md`): `rewrite`'s `input` is `{"text"}` for
`prompt_submit`/`turn_end`, the tool input for `before_tool`, `{"result"}`/`{"note"}` for `after_tool`; the gate's
environment is an allow-list with no token, socket or provider key; a skill binds a gate with
`metadata: {kelir-gate: <event>}`; `observe` prints the effective gates and what fired.

Example opinions, as gates (all M8, shipped under `examples/gates/`): verify-before-claim at `turn_end`; `note_check.py` at
`after_tool` on `edit`; commit and push at `session_end`, pull at `session_start`; numbered-item
answers ("do 1,4") resolved at `prompt_submit` against the last numbered list in the log. A
`bakeoff` (one prompt, two models, diff) is a script outside kelir driving two `kelir run --json`.

## Subagents

A child is a kelir process the model starts from `bash`: `kelir run --child "<task>" [--background]`
or `kelir brief "<question>"`. There is **no `spawn` tool**; the native tool list is `bash`, `edit`,
`compact`. `KELIR_ROOT`, `KELIR_PARENT` and `KELIR_DEPTH` ride along with `KELIR_SOCK` and
`KELIR_TOKEN` (exported to every bash child), so the CLI registers `child_started`/`child_settled`
with the **root session's socket** (the root stays the only writer of the registry and of its own log; each child still owns
its own session dir with `parent`/`depth` in `session_started`). A foreground child's result is the
bash call's stdout; a background child's `child_settled` arrives at the next boundary and wakes the
parent. Because a child is a session, `stop`, `resume`, `observe`, `attach` and `kelir ls`/kill work
on it unchanged. **Limits** (`config.toml`), enforced by the CLI against the root's registry: depth 3
(root is 0), 10 children per agent, 16 live descendants per root; a start past a cap exits non-zero
with a message the model can read. **No spend cap**: cost per session and per child is shown in
`kelir ls`, not enforced. Only the root reports Herdr status. `brief` is a child preset: a by-value
snapshot of the parent's context, `edit`/`compact` refused, history untouched. Delegation guidance
(when and how to start children) is a builtin skill injected at M8, not a tool.

## Background

`kelir bg run|ls|out|wait|cancel`. A job runs under a small shim (`kelir bg-shim`, the same
binary) that waits for the command, tees its output and writes `bg/<id>.status`, so it outlives
the session. Completion while the session runs appends `bg_settled` and admits a message at the
next boundary so the agent wakes; while it does not, see *Process model*.

A job's shim inherits `KELIR_SOCK` and `KELIR_TOKEN` from the session that started it (its
command line carries neither) and receives its signing key over a pipe, never in an environment
variable. A report with the token but not the job's capability, a status file the shim did not
sign, and a `cancel` for an id this session did not create are all refused (see *Who may drive a
session*).

## Jobs: named sessions, single-flight, STATUS

kelir has no daemon and no scheduler; `kelir job run <name>` is what a scheduler (cron, a
systemd timer, Task Scheduler, any trigger) calls. Reference: `docs/reference/jobs.md`.

- **Named sessions.** `kelir run --name <n>` binds `names/<n>` (`[a-z0-9._-]{1,64}`) to a
  session id, published create-or-fail and replaced by rename. An existing name **resumes** that
  session (M3 resume) and appends the task as a `user` event `from: "job"`; a name whose session
  is running is refused with the holder's pid (the session's `owner` file: pid + start time).
  `kelir sessions ls|rename|forget`.
- **Jobs.** `jobs.toml` (config dir, then the project's `.kelir/jobs.toml`): `task`/`task_file`,
  `provider`, `model`, `max_steps`, `timeout`, `session` (default: the job name), `new_session_every`
  (`never` or a duration: a fresh session when the old one is older or its context is large,
  carrying a summary as a `carry` source), `notify`, and `schedule`, a string for humans.
- **Single flight.** One lock per job (`jobs/<name>/lock`): a record (token, pid, process start
  time, command) published atomically, so an ownerless lock is never observed; taken over only when
  the holder is provably gone (pid reuse is a different start time), never from a young lock, only
  after two agreeing looks, and only if the record moved aside is the one judged; released only by
  its holder. A second run exits 75 at once (`--wait` queues), appends `skipped` to `runs.jsonl` and
  does not touch the holder's STATUS.
- **STATUS.** `STATUS.json` + `STATUS.md` written atomically at run start (`running`) and end:
  job, session, run, started/finished, outcome (`done|error|stopped|timeout|skipped|paused`),
  steps, tokens, cost (STEER-8), cache hit ratio, the first lines of the answer, the last error and the
  job's `schedule`. Exit codes: 0 done, 1 error/stopped, 2 usage, 75 skipped, 124 timeout.
- **The next day.** The system prompt is frozen at session start and holds the date, never the
  time. A resume on a later UTC day appends a `clock` source ("it is now <date>") at the tail; the
  cached prefix is not touched. Under `kelir job run` one `job` source line tells the model where
  STATUS and `notes/` are.
- **Trust.** `names/` and `jobs/` are owner-only directories; no key or token is written to
  either; notify hooks run gate-style (scrubbed environment, 30 s, tree kill, never change the exit
  code) and a project's hooks only in a trusted project.

## Scriptable, TUI and Herdr status

- `kelir run --json` streams the events as NDJSON on stdout and ends with a `result` event and
  an exit code. `kelir ask "<question>"` is the one-shot mode (cold start under 100 ms).
- The TUI is ratatui with an inline viewport: finished turns go to the terminal's scrollback via
  `insert_before`; only the input box and status line repaint. Every TUI action exists as a verb.
- Herdr status is OSC 7501 (`app=kelir`, root session only, a resume label): `working` at turn
  start, `done` on success, `idle` on stop, `error` with a `msg`; `idle` instead of `done` when
  `TERM_PROGRAM=herdr-telegram`. A turn paused on an explicit step cap reports `blocked`: it is the
  one state in which kelir waits on a human (saying `continue`), and the word this Herdr accepts
  (it drops `paused`; DEVIATIONS).

## Providers

One OpenAI Chat Completions client, and exactly two providers: `openrouter` and `opencode-go`
(`https://opencode.ai/inference/go`). `provider = "…"` (config, `KELIR_CONFIG`, `--provider`)
accepts only those two; anything else is a usage error that names them. There are no ad-hoc
provider names and no `kind = …`: the name *is* the provider. An optional
`[providers.openrouter]` / `[providers.opencode-go]` table only overrides that provider's settings
(`base_url`, extra headers, key source, `reasoning_field`, `reasoning_effort`, `session_header`,
`max_concurrent_requests`), and a table for any other name is the same usage error.

Go needs `x-opencode-session` and a `kelir/<version>` User-Agent. Cost (and Go's `allowance`) come
from the final usage chunk (`stream_options.include_usage`). The default model is
`deepseek/deepseek-v4.1-flash-1017` on OpenRouter (the maintainer's call, 2026-11-19); the Go
equivalent is `deepseek-v4.1-flash`.

**Key sources, first match wins** — see [`src/credentials.rs`]:

- **`openrouter`**: `OPENROUTER_API_KEY`, then `key_env` (a variable named in the table), then
  `key_cmd = ["cmd", …]` (its trimmed stdout).
- **`opencode-go`**: an explicit override first — `KELIR_GO_API_KEY`, then `key_env`, then
  `key_cmd` — and otherwise **opencode's own credential**, read through the supported
  `opencode auth export opencode` CLI (JSON array; the `integrationID == "opencode"` entry with
  `active: true`; token `value.access`, expiry `value.expires` in epoch ms). A store entry that
  is expired or inside its two-minute margin is read **once more** (opencode refreshes its own
  token when it runs) before an expired one becomes a usage error naming the expiry. No SQLite and no
  reading `opencode.db`; no OAuth refresh of kelir's own, because opencode rotates the refresh
  token and a second refresher would break the account it shares with kelir. The `opencode`
  executable is found on `PATH` (`.exe`/`.cmd`/`.bat`/`.com` on Windows, a `.cmd` shim through
  `COMSPEC /C`), run with no console window, no inherited stdin, a 5 s bound and the whole tree
  killed by pid if it overruns; its output is never echoed and is read up to 64 MiB, past which
  the read fails with a clear message. The dev box measures the read at
  ~0.33–0.39 s (worst cold ~1.7 s), so session start pays it once, before the first request.

A literal `key = "…"` in a config file is refused with a warning (it goes stale, shadows a live
store and copies a secret around) and is never used. A key is read once at session start, held in
memory only, and never written to the log, a status file, `observe`, an error message or the
process command line; `session_started.key_origin` records only the *source* (`env
OPENROUTER_API_KEY`, `key_cmd getkey`, `opencode auth export opencode`).

**Freshness.** A key whose source publishes an expiry (opencode's store does) is re-read before a
request when it is within two minutes of `expires`; a 401/403 re-reads the source once and retries
the request once with the new key. When it is still expired the turn fails with the expiry time and
the fix ("run any `opencode` command once so it refreshes its credential, then retry").

**`kelir doctor [--json] [--provider <name>]`** prints both providers (or one), their base URL,
which source answered, the expiry and the read time, and the fix when there is none — never a key.
It sends no request, so it is offline and free; exit 0 when every provider checked has a usable
key.

**Transient provider errors are ridden out, not fatal.** 429, 408, every 5xx
(incl. 529/loaded) and a request that dies before any tool call of the step began
are retried: `Retry-After` (seconds or HTTP-date) and the reset headers
(`x-ratelimit-reset*`, `anthropic-ratelimit-*-reset`) are honoured when present,
otherwise exponential with full jitter (base 2 s, factor 2, cap 120 s). The whole
request works inside one waiting budget (`retry_max_wait`, default 30 min, `0` =
unlimited); when it runs out the turn is **paused** (`turn_ended` outcome
`paused` with `reason: "provider"`, "provider unavailable …; say `continue` to
retry"), never failed, and the next `say` retries from the same fold. A hint
never shortens a wait below a quarter second, so a provider that says
`Retry-After: 0` for ever cannot turn "backoff" into a hot loop. Every wait
writes an `error` event
(`where: "provider"`, `retryable`, status, attempt, `retry_in_ms`) and a visible
line ("rate limited (429): retrying in 34 s, attempt 5, say stop to cancel"), and
it is interruptible: `stop`, `end` and a signal end the turn at once, `say
--now` and `model` apply at once and the request is re-sent immediately (with
the message in the fold, or on the new model and its provider's governor, and
with a fresh budget); `later`, `tools`, `skills` and `compact` keep their
boundary. A stopped or interrupted wait ends the turn `stopped`. 400/401/403/404
are never retried, and a retryable failure's `error` message is the status and
its class only — never a response body, which can echo a credential.
`[providers.<name>] max_concurrent_requests = N` is an opt-in client-side
governor: N advisory OS file locks under `<state root>/locks/`, so several local
kelir processes never overlap more than N requests on one account, and a crashed
holder leaks nothing. It follows the provider a request goes to, so a `model`
swap takes the new provider's slots (a compaction's summary request included).

kelir keeps no table of model facts. It reads the provider's own `GET {base}/models` —
`context_length` and `supported_parameters` on OpenRouter, `limit.context` and a `capabilities`
object on Go — and uses it for the window that drives auto-compaction. The list is cached in the
state directory for a day, a failed fetch is retried at most hourly, and the fetch is bounded, so a
start with a fresh cache makes no network call. `[model_windows]`/`window` still win (a person knows
what the provider cannot), and a model the list does not name falls back to 128 000;
`window_source` in every verb's `state` says which of the four answered. Details:  §11.

## Footprint and dependencies

Single binary ≤ 20 MB (strip + LTO), idle RSS ≤ 30 MB, no daemon, no SQLite (JSONL + in-memory
fold). Deps: tokio, serde, one HTTP client (reqwest + rustls), ratatui/crossterm, clap, rmcp
(client only), interprocess. Realistic size 8–15 k LoC. The heavy deps linked together measured
3.0 MB ( §D).

## Milestones

| # | Build | How you know it works |
|---|---|---|
| M0 | one-file loop: Chat Completions client (OpenRouter + Go) → stream → `bash` + `edit`, parallel calls → 10 KB spill → event log v0 (incl. `tool_started`/`tool_settled`, `usage` with cache fields) → NDJSON out; CI on Linux, macOS, Windows | the fixed M0 task (a scratch clone of a small public repo: add a CLI flag and its test, make the suite pass; ~20 steps) passes on both providers on host A and on host B; `kelir run --json \| jq` works; **`cached_tokens > 0` from step 2 on both providers**; Go sees `x-opencode-session` and a `kelir/…` User-Agent |
| M1 | verbs `say`/`later`/`stop` (incl. `--call`)/`model`/`tools` over the session socket | a mid-turn instruction is obeyed at the next step; `stop --call` on a running `sleep` and the turn continues; a model swap applies next step |
| M2 | inline ratatui TUI in the session process, plus the OSC 7501 writer | every TUI action is also a `ctl` verb; a kelir pane in the Herdr app shows working / idle / done |
| M3 | durability (fsync, unsettled detection, `resume`) and compaction (tool, `/compact`, auto at 80 %) | `kill -9` at five random points, resume never re-runs a completed command; after a compaction the task and every human message are verbatim in the next request |
| M4 | `bg` + shim + status files + 10-minute promotion + wake | a 15-minute command is promoted, kelir exits, `kelir bg out` still reads it, `resume` shows it settled |
| M5 | sources (machine + project, incl. the Herdr line), skills, the pushed hint, memory, attachments, `observe` | with `HC_TERMINAL_ID` set the frozen system holds one Herdr line (pane, space, tab, where) and without it none, `hc` missing fails open; `observe` matches what the provider received byte for byte; the hint names the right skill for 10 held-out prompts; the cache hit ratio holds across a hint |
| — | **dogfood 1:** kelir is the default for the maintainer's own terminal side tasks | a week of real use; the scorecard reads kelir's logs |
| M6 | `kelir mcp` (`rmcp` client, socket broker, `mcp_started`/`mcp_settled`) | `kelir mcp <server> <tool> … \| jq` works in a bash call; the call shows in `observe`; a >10 KB result spills |
| M7 | children as CLI (`kelir run --child`, `kelir brief`, root-registered via `KELIR_ROOT`/`KELIR_PARENT`/`KELIR_DEPTH`), caps, `kelir ls` and kill | a child of a child finishes; killing the parent leaves no orphan (Linux and Windows); the 17th live descendant is refused; `brief` hits the parent's cache |
| M8 | gates (six events), the maintainer's gates: verify, note-check, pull/commit, "do 1,4"; and the builtin skills `delegation` and `herdr` (`herdr`: `hc ls --json` with state/status/agent/name/where/cwd across machines, group `where` by space prefix to answer how many agents share my space, `hc get/read/send/wait`, `hc machine ls`, and the rule **never kill another agent's pane**; outside Herdr the skill says so) | a hedged final answer is blocked once; a crashing gate fails open; `observe` lists what fired; asked "how many agents share my space?" inside a pane the model answers from `hc ls --json`, outside one it says it is not in Herdr |
| M9 | jobs, named sessions, `STATUS` | a job resumes its session the next day; single-flight holds |
| M10 | Telegram: `tg-bridge` through `kelir mcp`, `notifications/claude/channel` inbound, `turn_done`, `kelir` as a third `/harness` in `tg-router` | a DM topic gets an answer and a fallback reply; the spool survives a restart |
| — | **dogfood 2:** one Telegram topic routed to kelir for a week | |
| M11 | `attach` viewer + notifications | `hc wait` sees kelir; the viewer is under 10 MB |

## Unverified (test at the milestone that needs it)

- Whether Go's reasoning-effort request field is `reasoning_effort` (M0).
- Whether `tg-router` needs the client to declare the `claude/channel` capability before it sends
  (M10). kelir does not declare `claude/channel/permission`: it has no approvals.
- Whether Gemini on OpenRouter needs explicit `cache_control` (OpenRouter's page says both; M0 if
  a Gemini model is in the config).
