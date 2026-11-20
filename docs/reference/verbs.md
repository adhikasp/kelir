# Verbs (`kelir ctl`, the socket, the TUI)

**Summary:** every verb a human, a script or a gate can send a running session,
what its arguments are, the boundary it applies at, and the event it writes.
**Read this when:** you are steering a session from outside (`kelir ctl`), or you
are adding a verb (`src/verbs.rs`: `Verb`, `parse`, `Boundary`).

A verb arrives as `{"verb": "say", "args": {…}}` on the session socket or as
`kelir ctl <session> <verb> [args]`. It is logged as a `verb` event first, then
applied at **one named boundary** — never between a tool call and its result.

## Verbs

<!-- generated:verbs -->
| verb | arguments | applies at | what it does |
|---|---|---|---|
| `say` | `<text>` or a JSON object with `text`; `--now` | `now` (with `--now`) or `queued` (the next boundary) | injects a user message (`user` event, `delivery` `now`/`next`); `--now` stops the running calls first |
| `later` | `<text>` or a JSON object with `text` | `turn_end` | queues a user message for the end of the turn (`user` event, `delivery` `later`) |
| `stop` | `[reason]`; `--call <id>` | `now` | stops the turn: every running tool process tree is signalled and its call settles `stopped`; `--call` stops one call and the turn continues |
| `model` | `<provider>/<model>` or `<model>` | `next_step` | swaps provider/model for the rest of the session; the next `step_started` carries the new one |
| `tools` | `--disable <name>` / `--enable <name>` (repeatable) | `next_step` | disables or enables a native tool; the tool list sent to the provider never changes, a disabled tool's call settles `disabled` |
| `skills` | `--name <n> --path <file-or-dir>` or `--text <body>`; `--kind skill\|instruction` | `next_step` | adds or replaces one source; it arrives as a `source` message at the tail, never as a rewrite of the frozen system prompt |
| `compact` | `[focus]` | `next_step` | summarises the earlier context at the next boundary and writes a `compacted` event; the task and the messages a human typed stay verbatim |
| `view` | `<path>` | `now` | queues an image for the next message: a `source` event of kind `attachment` at the tail, so the cached prefix is untouched; an audio or video file is stored under the session and noted (kind `attachment-note`), not sent |
| `bg` | one job status object, as `kelir bg run` sends it | `now` | a job reported itself or changed state; the session writes `bg_started`/`bg_settled` and, for a settled job, the wake-up `user` message |
| `cancel` | `<job id>` | `now` | kills one of this session's background jobs and records it `cancelled`; a job id                  the session did not start is refused |
| `status` | none; `--json` prints the answer as one JSON line | `now` | read-only and never logged: answers at once, even mid-request or in a retry wait,                  with steps used/limit, turn, last prompt size and context %, cache hit ratio,                  tokens, cost and its source, last error, retry in progress, running calls,                  background jobs, children. `context.window` is `null` (with `context.window_source` `default`) until the model list answers or a window is configured, and `percent` stays `null` with it. A provider-reported cost of exactly 0 on a priced model is replaced by a table estimate (`cost.source` `table`). Needs the session token like every verb |
| `end` | none | `now` | ends the session (`session_ended`); running calls are stopped first |
<!-- /generated -->

## The boundaries

| boundary | when |
|---|---|
| `now` | immediately: the running tool calls are stopped first, `[Skipped]` results are given for the rest, and the turn continues from there |
| `next_step` | just before the next provider request |
| `queued` | the next boundary of any kind (a `say` that must not interrupt) |
| `turn_end` | after the step that carried no tool calls |
| `rejected` | never: the `verb` event carries an `error` and nothing else happens |

## `kelir ctl` and a session that is not running

`ctl` finds a session by id, by a unique prefix, or `latest`. It reads the
session's socket address from `<state>/sessions/<id>/sock`; a session that is not
running is an error with exit 1, not a queue: a message to a stopped session is a
`kelir resume <id> "<message>"`.

## Who sends what

| caller | `from` |
|---|---|
| `kelir ctl` | `ctl` |
| the TUI's `/` commands | `tui` |
| `kelir view` (from a tool call) | `tool` |
| a background job reporting itself | `job` |
| a gate | `gate` |

## Adding a verb

1. `Verb` gets the variant and `parse` its name and arguments (validate at parse
   time: `kelir ctl` should refuse a bad invocation itself).
2. `Verb::scope` and `Verb::applies_at` say where it lands.
3. The `docs/tables.rs` `VERBS` row says what a reader needs.
4. The apply arm in `src/session.rs` (`apply_verb`) writes the event and says how
   it landed.
5. `cargo test --lib docs` proves the table and the parser agree.
