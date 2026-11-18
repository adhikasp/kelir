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

- **The OSC 7501 record is exactly `ESC ] 7501 ; state=<word> ESC \` — nothing
  else.** Herdr `0.1.21` accepts only that shape. M2's first writer carried
  `app=kelir[;id=<session>][;msg=<base64>]` (what `claude-code` emits and what
  the spec's "Herdr status" paragraph describes); on this Herdr every record
  with an extra key, a trailing `;` or another separator is dropped whole and
  the pane's `status` stays `null`, so the milestone's acceptance — a kelir pane
  shows working / done — did not hold (M2 review B1). Measured, not assumed:
  `scripts/live/m2_osc_probe.py` in plain panes sent each variant and `hc get`
  showed `{'state': 'working'}` only for the bare form (also on a Linux core);
  `scripts/live/m2_status_check.sh` then drove the real TUI in a pane and
  `hc get` flipped `working` → `done`. Consequences, recorded because they
  differ from the spec: no `app=kelir` identity, no `id=<session>` resume
  label, no `msg` on `error` (the reason is in the log and on the pane's own
  screen). A second, labelled record is ignored by 0.1.21 but does not clear
  the status either; it is not sent until a Herdr version accepts it, since
  nothing reads it today.
- **`kelir --model x` is `kelir tui --model x`.** The TUI's flags (model,
  provider, cwd, config, resume) are flattened onto `kelir` itself, so the
  default surface takes them; flags given before the subcommand fill in what
  `tui` did not set (`kelir --model a tui --provider b`). `tui` has no task
  positional: the first message is typed in the editor, and a word after `tui`
  is a usage error from the parser (M2 review N2).
- **`KELIR_TEST_PANIC=1` is a test-only injection.** When set, the TUI panics
  right after its first paint (raw mode on, bracketed paste on, a painted
  viewport), which is the path a real crash takes. `tests/e2e_tui.rs` uses it
  to assert the restore, and `scripts/live/m2_status_check.sh` with
  `M2_CHECK_INPUT=1` checks in a Herdr pane that the shell still echoes a typed
  line afterwards. Nothing else reads the variable; a normal run never panics
  on purpose.
- **The status writer is on for `kelir run` too**, not only the TUI: whenever
  stdout is a TTY (or `KELIR_HERDR=1`) and `--json` is off. A headless agent in
  a Herdr pane is still an agent. `--json` is always excluded, because NDJSON on
  stdout must stay parseable; `KELIR_HERDR=0` turns it off. While the TUI runs,
  only the TUI task writes to stdout — the session hands it the record as a
  `UiEvent` and it is written between two paints — so an OSC can never land
  inside a crossterm CSI sequence (M2 review F2).
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
  idle TUI would eat the next message. It is still logged as a `verb`
  (`boundary: "dropped"`), the verb's answer says `applies: "dropped"` rather
  than `now` — for `ctl` clients and the TUI alike — and the UI is told
  "nothing is running" (M2 review N1).
- **The inline viewport is a fixed 8 rows** (four live rows, a three-row editor,
  the status line), and a terminal shorter than that gets fewer. Streaming text
  that no longer fits is written into the scrollback as it arrives, so a long
  answer scrolls like normal terminal output instead of growing the repaint area.
- **The TUI ends the session if it dies, and leaves the shell a clean
  terminal.** A panic or a terminal error in the UI task sends the `end` verb,
  so a broken UI cannot leave a pane hanging with no way in. On both paths —
  `/quit`, a double Ctrl-C, and a panic — the viewport rows are erased (never
  the scrollback above them), the cursor is shown and put on the first free row
  below, bracketed paste is turned off and raw mode is dropped, so the shell's
  next output starts on a new line instead of overwriting the banner (M2 review
  F3, N5). The panic hook does this itself and then ratatui's hook; the normal
  exit does not call `try_restore`, which would emit `LeaveAlternateScreen` for
  a screen the inline viewport never entered.
- **A TUI session starts with no task and no `user` event.** `session_started`
  is written first; the first turn begins when the first `say` arrives, so the
  log and the request sequence are the same as any other session's.

## M3

- **`resumed.from_seq` is the seq of the last event before the resume**, i.e.
  the point the log continues from (0 when the log only has `session_started`).
  The resume event's own seq is the first one the resumed process writes.
  `unsettled` lists the call ids in the order the model asked for them.
- **A call with no result is settled `unknown`, never re-run.** Every
  `tool_started` without a `tool_settled` *and* every `tool_calls` entry of an
  assistant message without one gets exactly one synthetic `tool_settled` with
  `outcome: "unknown"` and the result `unknown outcome: kelir was interrupted
  while this call was running; it may or may not have completed. Check before
  retrying.` When the interrupted call had already written output, one more line
  follows: `partial output: N bytes, M lines; full output at <path>` (the
  `commands/<seq>-*` directory the log names). The second class — asked for but
  never started, which is the crash right after `usage` — is settled too,
  because an assistant tool call with no `tool` message is a request every
  provider rejects; its sentence is the same, since it may or may not have run.
- **The tool list of a resumed session is the one in `session_started`.** The
  specs are filtered to the recorded names, so a session started before
  `compact` existed keeps its two-tool list and its cache prefix. A disabled
  tool is *not* replayed into a resume (the list never changed anyway, but a
  `tools --disable` is forgotten).
- **A resume takes its id, cwd, provider, model and counters from the log, not  from flags.** `--cwd`, `--model` and `--provider` are ignored with `--resume`;
  the config (windows, prompts, keys) is today's machine's. The model/provider
  are the last `step_started`'s, so a `model` verb swap survives the resume.
  `step` and `turn` continue from the log instead of restarting.
- **`kelir resume <id>` on a finished session with no task does nothing.** It
  appends the `resumed` event and one `session_ended {reason: "resumed"}`, exits
  0 and makes no provider request. With a task (`kelir resume <id> <task>`,
  `kelir run --resume <id> <task>`) the message is appended (delivery `next`
  when a turn was in flight, so it lands after the unknown results) and one turn
  runs.
- **A `SIGKILL`d session cannot clean up its running calls.** Windows closes the
  Job Object when the process dies, so the call's tree goes with it; on Unix the
  call's own process group (`setsid`) is not a child of anything the session can
  still signal after `SIGKILL`, so a long command can outlive the log. kelir
  never kills a call it cannot reach; the resume reports that call as unknown,
  which is exactly the case the unknown outcome exists for.
- **A live session is refused, not taken over.** `resume` connects to the
  session socket first; if something answers, it exits 2 with `session … is
  already running`. A stale `sock` (and a `sock.path` pointer) left by a dead
  process is replaced by the new bind — `ipc::bind` already removed the stale
  file on Unix, and a Windows pipe name does not outlive its process.
- **A crash leaves no `turn_ended` and no `session_ended`.** That was already
  true and is unchanged: the log simply stops. A resume does not invent them; it
  appends `resumed`, settles the unknown calls and continues the turn, and the
  `turn_ended` in the log after that is the resumed turn's own. A resume
  continues when the log was **not closed cleanly** — a turn with no
  `turn_ended` after its last step, or (a kill between two turns) no
  `session_ended` at all — so a `kill -9` at any point during a task resumes it.
  A session that ended cleanly (`done`, `stopped` or `error`, then
  `session_ended`) continues only when the caller passes a new task.
- **`compacted.cache_restart` is a field the spec's table does not list.** The
  spec asks that the cache restart is logged; the event carries
  `cache_restart: true` (it is inherent), the UI says so, and the next
  `step_started.request_hash` differs because the prefix was rewritten. What is
  *not* logged is the summarising request's own `usage`/cost: it is not a step,
  so it has no step number to record and `kelir ls` does not count it yet.
- **`tokens_before`/`tokens_after` are estimates**: four bytes per token, rounded
  up, over the JSON the provider would receive (system message included, tools
  not). The auto trigger uses the provider's own `usage.prompt_tokens` when the
  log has one, and the same estimate otherwise (the first step, and the step
  after a compaction).
- **Pins: the first `user` message whatever its `from`, plus every `user` whose
  `from` is a human.** `tui`, `ctl`, `cli` and `channel` count as a human (a
  person typed it, in the editor, in a shell or in a chat bridge); `job` and
  `gate` do not. Pins are rendered after the summary, in log order, as the
  original `user` messages, verbatim (a spilled one is read back from its blob).
  The summary itself arrives as one `user` message:
  `[kelir compacted the earlier conversation]\n\n<summary>`.
- **A compaction keeps a tail verbatim** (`compact_keep`, default 4 messages) so
  the model still sees what it was doing; `fold` cannot cut between an assistant
  tool call and its result, so the cut moves back to that assistant when a
  `tool_settled` would otherwise start the tail. A second compaction supersedes
  the first (its summary and pins are inside the second's range, so they are
  dropped) and re-pins the human messages it still finds in the log.
- **The auto trigger never fires before the log's first step**, and only when a
  plan exists: at the first `before_step` the "conversation" is the task alone,
  and compacting it would cost a provider call to save nothing.
- **The summarising request is sealed**: the `compact_prompt` as the system
  message and one user message (the focus line, then the transcript of the
  range), built by the same client — so it carries the session id and can hit
  the parent's cache — with **no tools**. Verbs are not read while it is in
  flight; a `stop` arriving then is applied at the next boundary, which is the
  same boundary that would have sent the next step.
- **`compact` applies at `before_step` only**, from all three triggers: the
  tool's result says `compaction queued …`, the verb is queued like `model`, and
  the auto trigger sets the same request. `kelir ctl <id> compact <focus>` now
  carries the focus (before M3 the CLI dropped the words for this verb).
- **`KELIR_CRASH_AT=<point>` is a test-only injection.** A scripted test names
  one of `tool_started` (after the intent is fsync'd, before any process
  starts), `mid_tool` (150 ms after the calls start, side effects already made),
  `tool_settled` (after the batch is committed), `mid_stream` (on the first text
  delta, nothing committed) or `usage` (after the assistant and its usage are
  committed, before the tool calls). It prints one line to stderr and calls
  `std::process::abort()`; any other value, and an unset variable, do nothing.
  The process really dies (SIGABRT on Unix, `0xC0000409` on Windows), which is
  what lets `tests/e2e_m3.rs` prove a resume does not re-run a command.
- **fsync is per batch, and a batch is atomic on disk.** `EventLog::sync` (flush
  + `sync_all`) runs after every batch, after every verb ack and after the
  `resumed`/unknown block. A reader that never saw the writer's memory sees a
  whole number of events ending on a newline (`a_batch_is_whole_on_disk…`). What
  a unit test cannot prove is the fsync itself under a power cut; the crash hook
  proves the ordering and that committed events survive the process dying. A
  torn last line is reported on resume as `error {where: "log"}`, and a final
  line that is a whole event without its newline is kept rather than dropped.
- **The TUI no longer double-prints tool lines.** The session printed
  `→ bash … · ok · N B` on stdout while the TUI painted its own line for the
  same event; that write is now headless-only (`!json && !tui`), so the painter
  stays the only writer to the terminal.
