# Deviations from `docs/SPEC.md`

Small, deliberate differences, with the reason. Everything else tracks the spec.

## M0

- **`user.from` may be `cli`.** The spec lists `tui`, `ctl`, `channel`, `job`,
  `gate`. `kelir run` / `kelir ask` are the scriptable surface, so their user
  events carry `"from": "cli"` instead of pretending to be one of the five.
- **OpenCode Go base URL is `https://opencode.ai/inference/go/v1`.** The spec
  writes `https://opencode.ai/inference/go`; the OpenAI-compatible endpoint is
  `…/go/v1/chat/completions`, and the base URL is configurable, so either form
  can be set in `config.toml` (a base URL already ending in
  `/chat/completions` is used as-is).
- **M0's tool list is `bash` and `edit` only.** `spawn` and `compact` are fixed
  into the list at M7 and M3; the list stays constant for the whole session, as
  the cache requires.
- **`spill_limit` is configurable** (`config.toml`, default 10240). The spec
  fixes 10 KB; a knob makes the boundary testable without faking sizes.
- **`KELIR_STATE_DIR`, `KELIR_CONFIG`, `KELIR_CONFIG_DIR`, `KELIR_BASE_URL`.**
  Environment overrides so the test suite (and a proxy or the mock server) can
  isolate state and point a provider at a stand-in. Nothing else reads them.
- **`result` is a CLI event, not a log event.** `kelir run --json` ends its
  NDJSON stream with `{"type": "result", …}`; it is never appended to
  `events.jsonl` (the log's last event is `session_ended`). It is printed for
  usage errors too (exit 2), so a scripted caller always gets one.
- **Payloads over 10 KB are spilled to `blobs/<seq>-<field>`.** The event holds
  `{"spilled": true, "path": …, "bytes": N, "preview": "<first 1 KB>"}` in place
  of the string or argument value, and `fold` reads the file back, so the
  request bytes (and the cache prefix) are identical to the inline form. This
  covers `user.text`, `assistant.text`/`reasoning`, `tool_calls[].args`,
  `system.text`, `tool_started.input`, `tool_settled.result` and
  `turn_ended.final_text`. `session_started.system.text` is folded from the blob
  like the rest.
- **Command output directories are `commands/<seq>-<step>-<n>`.** `seq` is the
  `tool_started` event's seq, so ids repeat safely (some providers send
  `call_0` every step) and a provider-supplied call id can never reach the
  filesystem. The call id stays a label in the events; `out_path` points at the
  real, never-truncated file.
- **`bash` returns when the shell exits, not at pipe EOF**, after a 200 ms drain
  for output already in the pipe. A leftover background child keeps running and
  is not tracked (that is `bg`, M4). Its process group is held by the session so
  it is not killed by the call returning; closing the session closes the group.
- **N2 (fixed in M1): the child is created suspended** (`CREATE_SUSPENDED`),
  assigned to its Job Object and only then resumed, so it cannot run user code
  before it is inside the job. `std` does not expose the thread handle, so the
  thread is found through a `CreateToolhelp32Snapshot` walk of the (still
  suspended) process. Unix needs nothing extra: `process_group(0)` is applied
  before `exec`.

## M1

- **The `verb` event carries two fields the spec's table does not name:**
  `boundary` (`now`, `next_step`, `queued`, `turn_end`, or `rejected`) and, when
  a verb was refused, `error`. They are what tells a reader of the log where the
  verb applied or why it did not; `name`, `args` and `from` are as specified.
- **`kelir ctl` validates a verb locally.** A malformed invocation (unknown verb,
  `say` with no text, an unknown tool name) exits 2 without a socket round trip,
  so nothing is logged: the request never reached the session. A verb the
  *session* refuses (`stop --call <id>` for an id that is not running) is logged
  with `"boundary": "rejected"` and answered `{"ok": false, "error": …}`.
- **`model <target>` splits only on a configured provider name.** `model
  openrouter/deepseek/deepseek-chat` switches the provider *and* the model;
  `model deepseek/deepseek-chat` (whose first segment is not a provider) keeps
  the current provider and replaces the model id, which is the common case on
  OpenRouter, where model ids contain `/`.
- **`step` is a session counter, not a turn counter.** In a `--keep` session the
  second turn starts at the step after the first turn's last one, so `step` stays
  unique in one log. `max_steps` still counts the steps of one turn.
- **`later` while the session is idle starts a turn at once.** There is no turn
  to end; the message is recorded with `delivery: "later"` and the process runs a
  turn for it. Likewise a `say` still pending when a turn ends opens the next
  turn instead of being dropped.
- **`end` is a session verb, not a spec-table verb.** `kelir ctl <s> end` logs
  `session_ended` and exits 0 (after a `turn_ended outcome=stopped` if a turn was
  running). It exists for `--keep` sessions, which would otherwise have no way to
  stop.
- **A disabled call's outcome is `disabled`,** with the result text `disabled by
  user: …`. The `tools` array in the request is untouched, as the cache requires.
  A `stop` while calls run settles them as `stopped` (a process was running) or
  `skipped` (the task never started) — both readable by the model.
- **`stop --call` has its own grace window, never the batch's.** The verb kills
  one call's process tree, that call alone gets the 1.5 s window in which to
  report back, and the batch waits for every other call to settle on its own.
  `live_calls` holds only the calls that have not settled, so `stop --call` for
  an unknown **or already settled** id is refused (`no running call with id …`)
  and changes nothing.
- **A stopped call keeps its partial output.** Its result is `stopped by user:
  <reason>`, then `partial output: <bytes> bytes, <lines> lines; full output at
  <path>`, then the last 1 KB of the file under `--- tail ---`; the
  `tool_settled` event carries the same `bytes`, `lines` and `out_path`. A call
  that printed nothing keeps the plain `stopped by user` result. The result is
  still far under the 10 KB spill limit, so it is never itself spilled.
- **`ctl latest` is the newest session and only it.** When that session is dead
  the verb fails with `session … is not running` instead of falling through to
  an older live session (where `latest end` could kill the wrong one). An exact
  id and a unique prefix resolve as before.
- **Unix: the state and session directories are 0700 and the socket is 0600.**
  `say` is a prompt-injection channel into an autonomous agent, so a umask like
  `002` must not let the group write to it. Windows keeps the default named-pipe
  DACL, which already denies other users write.
- **`stop` and `end` are read during the provider retry backoff.** The backoff
  is a `select!` like the stream, so a verb does not wait 200–800 ms for the
  retry to be attempted.
- **A failed turn does not end a `--keep` session.** A provider refusal (for
  example a mistyped `ctl model`) is logged as `error` and `turn_ended
  outcome=error` as before, but the process keeps serving verbs and the next
  `say` runs another turn; the exit code stays 0 when the session is later
  ended with `end`. A one-shot run still exits 1.
- **A call that cannot be put in a Job Object is refused, not run unmanaged**
  (M1 review N7). On Windows the child is created suspended, so a failed
  `AssignProcessToJobObject` (or a Job Object that cannot be created) terminates
  it and returns a readable `error` result instead of leaving a process that
  `stop` cannot reach. `kill_on_drop(true)` covers the remaining case of a call
  task cancelled after its tree was killed.

## M2

- **The OSC 7501 record format, as written.** `ESC ] 7501 ;` then `;`-separated
  `key=value` pairs, terminated by ST (`ESC \`, safer than BEL inside a TUI):
  `state=<idle|working|done|error>;app=kelir[;id=<session id>][;msg=<base64>]`.
  This is what `claude-code` emits (its writer was read out of the binary:
  `state` first, then `app`, then `id`/`kind`/`progress`/`title`/`msg`, with
  `msg` and `title` base64 because the payload may not contain a space or a
  `;`), and it is what the Herdr core on this machine parses (`crates/herdr-core/
  src/status.rs`: `state`, `msg`, `progress`, the words `idle, working, done,
  blocked, error`, both terminators). `hc get <terminal>` decodes `msg`, so the
  encoding is observable rather than assumed. Every change writes **two**
  records: one with no `id` (Herdr's *root* record — the pane's `status`, which
  is what the app draws) and one with `id=<session id>` (an entry in `records`,
  which is the "resume label" Herdr's resume mods read). kelir never sends
  `blocked`.
- **The status writer is on for `kelir run` too**, not only the TUI: whenever
  stdout is a TTY (or `KELIR_HERDR=1`) and `--json` is off. A headless agent in
  a Herdr pane is still an agent. `--json` is always excluded, because NDJSON on
  stdout must stay parseable; `KELIR_HERDR=0` turns it off.
- **`compact` is a parsed verb from M2, with the compactor in M3.** The TUI's
  `/compact` must be a verb like every other action, so `verbs::parse` accepts
  `compact [focus]`; applying it records an `error` event (`compact is not
  implemented until M3`) and the session carries on, exactly as a refused
  `model` swap already does. M3 replaces the apply arm.
- **`kelir tui --resume <id>` parses the flag and refuses it** with a usage
  error until M3 implements resume. The flag exists so a pane launcher can be
  written against the surface the task names.
- **A TUI action is a verb; a view is not.** Every action that changes the
  session (`submit`, `/stop`, `/say-now`, `/later`, `/model`, `/tools`,
  `/compact`, `/quit`, Ctrl-C, double Ctrl-C) is a `commands::Action` and goes
  out as one verb on the same channel the socket feeds, with `from: "tui"`.
  Three things are local views with no session state and therefore no verb:
  `/help`, the output tail (Ctrl-O), and editor movement. The enumeration test
  covers the `Action` enum, which is exhaustive by construction.
- **Enter sends, Alt-Enter or Shift-Enter adds a line, Ctrl-J adds a line
  everywhere.** A terminal does not distinguish Ctrl-Enter from Enter, so Ctrl-J
  (the bare line feed) is the portable way to type a newline. Bracketed paste is
  enabled while the TUI runs, so a pasted block arrives as one event (with its
  newlines) instead of a burst whose first newline would submit it.
- **`stop` with nothing running is dropped in idle, not queued.** A queued stop
  would apply at the next turn's first boundary and kill it, so Ctrl-C in an
  idle TUI would eat the next message. It is still logged as a `verb`; the UI is
  told "nothing is running".
- **The inline viewport is a fixed 8 rows** (four live rows, a three-row editor,
  the status line), and a terminal shorter than that gets fewer. Streaming text
  that no longer fits is written into the scrollback as it arrives, so a long
  answer scrolls like normal terminal output instead of growing the repaint area.
- **The TUI ends the session if it dies.** A panic or a terminal error in the
  UI task sends the `end` verb, so a broken UI cannot leave a pane hanging with
  no way in; the terminal itself is restored by ratatui's panic hook.
- **A TUI session starts with no task and no `user` event.** `session_started`
  is written first; the first turn begins when the first `say` arrives, so the
  log and the request sequence are the same as any other session's.


