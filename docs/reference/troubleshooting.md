# Troubleshooting

**Summary:** the failures that actually happen — a 429 that will not go away, a
turn paused with no error, a socket left by a dead process, a job nobody is
watching, a Windows console that eats a keystroke — and what each one means.
**Read this when:** something is stuck, slow, or "did nothing", and you want the
short list of causes before reading source.

Start with two commands: `kelir doctor` (which key each provider would use, never
the key) and the session's own log (`<state>/sessions/<id>/events.jsonl`, or
`kelir run --json`). Almost every question below is answered by one `error`,
`turn_ended` or `call_settled` event.

## "It says rate limited and waits"

`429` (and any other retryable status) is retried with backoff, honouring the
provider's `Retry-After`, inside a 30-minute budget per request
(`docs/reference/providers.md`). While it waits: the TUI's facts row counts down,
a dim line goes to the scrollback, `kelir run` prints it on stderr, and each
attempt is an `error` event with `status`, `attempt` and `retry_in_ms`. When the
budget runs out the turn **pauses** rather than failing: `turn_ended` with
`reason: "provider"`, exit code 0. `say continue` (or a new message) starts a new
turn. `stop` ends the wait now.

## "The turn stopped at N steps and calls it paused"

That is `max_steps` (or `--max-steps`) doing its job: the turn is capped, not
failed. `turn_ended` carries `reason: "steps"`; the next message starts a new
turn with a fresh count. Remove the key if you did not mean to cap it (`0` and
unset both mean no cap).

## "There is a sock but no session"

The session socket is a file on Unix and a pipe name on Windows; a session that
was killed leaves the file behind. `kelir ctl` says the session is not running
and exits 1 — it does not wait. Pick the session up with `kelir run --resume <id>`: it
binds the socket afresh and replaces a stale one. A call that was started and never settled is
settled in the log as `call_settled{kind:…, outcome:"unknown"}`; it is reported to the model
as an unknown outcome and **never re-run**.

## "A background job I started is gone"

A job outlives the session on purpose: its shim writes `calls/<id>/<id>.status`,
nothing wakes a stopped session. `kelir bg ls` (inside a session) and
`kelir bg out <id>` read it; the next `run --resume` folds the settled job into the log
as `call_settled{kind:"bg"}` plus the wake-up message. A job killed with the machine is the
one case with no status file: `bg ls` does not list it at all (no status file, no row) and the log has a
`call_started{kind:"bg"}` with no settle.

## "A child start was refused" or "a child is gone"

`kelir run --child` exits 3 with `refused: …` and the count when a cap is full
(`max_child_depth`, `max_children`, `max_live_descendants`; `docs/reference/children.md`).
`kelir ls` shows what is live; `kelir kill <session>` frees a slot. A child whose process
died shows `lost` (or `killed`) in `kelir ls` and in the root's `call_settled{kind:"child"}`. A child
ends with its parent: after `kill -9` of a Unix parent it takes up to a second to notice.
Read a child's trouble in `<state>/sessions/<child>/host.err` and its own log.

## "The model does not know a rule I wrote in AGENTS.md"

Three causes, in order: the file is above the repository root or above home (the
walk stops at both, `docs/reference/sources-and-skills.md`); the 256 KB total
instruction budget was spent, which is one `error` event per skipped file, with
the numbers; or the rule arrived *after* the session started, in which case it is
a `source` message at the tail and the frozen system prompt genuinely does not
contain it — that is the cache working as intended.

## "A tool call's output was cut"

It was not: the whole output is `calls/<call-id>/out` from the first byte, and
what the model saw is `call_settled.result`, which is a head/tail excerpt once the
output passes `spill_limit` (10 KB). `spilled: true` and `out_path` in the same
event say so. The model is told the path; a `bash` call can read the rest.

## "A gate did not do anything"

Gates fail open, so a broken one is silent to the model. Look at the log:
`kelir observe <session>` ends its gate section with `== gates fired ==`, and the
`gate` events (`jq 'select(.type=="gate")' events.jsonl`) carry `error`: `exited
with N: <stderr>`, `timed out after N ms`, `not JSON (...)`, `output over ...`, or
`cannot start the gate`. Test it by hand with the event on stdin
(`echo '{"event":"turn_end","final_text":"x"}' | python gate.py`). A gate gets a
**scrubbed environment**: no provider key, no `KELIR_TOKEN`, not even your
`PATH`-adjacent extras; put what it needs in its `env` table (`kelir docs gates`).

## Windows

- **A console window flashes.** No spawn path may create one: every child of a
  console-less parent is started with `CREATE_NO_WINDOW`. A visible window is a
  bug, not a setting.
- **Ctrl-C is not the TUI's.** A tool call runs in its own process group; a
  console control event sent to a group shared with the session would kill the
  session itself (that happened, twice, on 2026-11-19). Nothing kelir ships sends
  one; tests signal an exact pid or process group.
- **A `.cmd`/`.bat` helper needs `cmd.exe`.** `opencode` on an npm install is a
  `.cmd` shim, which `CreateProcess` cannot run directly: `COMSPEC` is used to
  invoke it, with the path quoted twice so `cmd /C` does not eat the first word.
- **A long command line is not passed on the command line.** A `bash` call's
  script goes to a file beside the output (always, on Windows), because the limit
  is 32 KB and MSYS rewrites backslashes in argv. That is why a 40 KB heredoc
  runs.

## Secrets

- A provider key is read, handed to the request builder and recorded as its
  *source* — never printed, logged or put on a command line. `kelir doctor`,
  `session_started.key_origin` and the one-line notice all name the source.
- A literal `key` in a config file is refused with a warning: it goes stale,
  shadows a live one, and puts a secret where it gets copied around.
- A provider (or a gateway) can echo a request; a 4xx that quotes the
  `Authorization` header is redacted before it reaches the log.
- `key_cmd` is named by its file name only, so a private path stays private.

## "The docs are stale"

There is no generator to run: `docs/reference/` is hand-maintained markdown.
`cargo test --lib docs` reads the code and fails when a verb, config key, event
type or environment variable exists there but is missing from the reference.

## "What will the model see?"

There is no print command. The request is the fold of `events.jsonl`:
`session_started.system` is the frozen system prompt (with its `sources[]`),
then the `user`/`assistant`/`call_settled` events in order, minus what a
`compacted` event replaced. Read the log (`kelir docs files`) to see it.

## "Rotate or replace a credential"

Change the first source in the key order of `kelir docs providers` for the
provider in use: set the new value in `OPENROUTER_API_KEY`/`KELIR_GO_API_KEY`
(or the `key_env` variable, or what `key_cmd` prints), or re-login with
`opencode` for the store. `session_started.key_origin` says which source won.
