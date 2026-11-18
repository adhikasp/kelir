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
  session socket first; if something answers, it exits 1 with `session … is
  already running`. A stale `sock` (and a `sock.path` pointer) left by a dead
  process is replaced by the new bind — `ipc::bind` already removed the stale
  file on Unix, and a Windows pipe name does not outlive its process.
- **On Unix "already running" is an exclusive `flock` on
  `<session>/session.lock`, not only the connect check.** `bind` unlinks a
  stale socket before listening, so connect-then-bind is two steps and two
  resumes started together could both pass it (M3 review N3). The session
  process holds the lock for its whole life; the loser exits 1 before it
  writes a byte. Windows keeps the named pipe's first-instance rule (and a
  no-op lock guard). A log that already ends with `session_ended` belongs to a
  process that is tearing down (`end` acks before the socket closes), so a
  resume waits up to 2 s for the socket to stop answering instead of reporting
  a race as "already running".
- **A crash leaves no `turn_ended` and no `session_ended`.** That was already
  true and is unchanged: the log simply stops. A resume does not invent them; it
  appends `resumed`, settles the unknown calls and continues the turn, and the
  `turn_ended` in the log after that is the resumed turn's own. A resume
  continues when the log was **not closed cleanly** — a turn with no
  `turn_ended` after its last step, or (a kill between two turns) no
  `session_ended` at all — so a `kill -9` at any point during a task resumes it.
- **A turn that ended `stopped` or `error` is continued by `resume`** with no
  new task (M3 review S1: SPEC's `resume` continues a stopped or crashed
  session, and a provider outage is the commonest reason to resume). A turn that
  ended `done` and was closed with `session_ended` still needs a new task.
  A resumed turn that fails on an error the provider calls retryable is tried
  again — the turn, not the step — while the provider attempt budget
  (`provider::MAX_ATTEMPTS`, three) lasts, because surviving an outage is what
  the resume is for. The one-shot exit code is that of the last attempt.
- **A resumed request never ends on an assistant message.** When the log was not
  closed and its last rendered message is the assistant's answer, the resume
  appends one `user` event — `[kelir] the session was interrupted between turns;
  continue the task from where it stopped.` (`from: "cli"`) — and runs a turn for
  it. A trailing assistant message is prefill to some providers and a rejection
  to others, and either way it is a paid, meaningless turn (M3 review S4).
- **`compacted.cache_restart` is a field the spec's table does not list.** The
  spec asks that the cache restart is logged; the event carries
  `cache_restart: true` (it is inherent), the UI says so, and the next
  `step_started.request_hash` differs because the prefix was rewritten.
- **The summarising request's spend is a `usage` event with `kind: "compact"`.**
  It is not a step, so it carries the *last* step number and the marker; a
  reader (or a future `kelir ls`) tells the two apart by `kind` and the numbers
  still add up (M3 review N4). Retrying a summary buys nothing — the provider is
  down or the prompt is too large — so it is exactly one request, unlike a
  step's three attempts.
- **`tokens_before`/`tokens_after` are estimates**: four bytes per token, rounded
  up, over the JSON of the whole request the provider would receive (system
  message included, tools not) — both sides of the comparison are the same
  context's, so "did the compaction shrink it?" is a real question. The auto
  trigger uses the provider's own `usage.prompt_tokens` when the log has one,
  and the same estimate otherwise (the first step, and the step after a
  compaction).
- **Pins: the first `user` message whatever its `from`, plus every `user` whose
  `from` is a human.** `tui`, `ctl`, `cli` and `channel` count as a human (a
  person typed it, in the editor, in a shell or in a chat bridge); `job` and
  `gate` do not. Pins are rendered after the summary, in log order, as the
  original `user` messages, verbatim (a spilled one is read back from its blob).
  The summary itself arrives as one `user` message:
  `[kelir compacted the earlier conversation]\n\n<summary>`.
- **A compaction renders at `through_seq`, not where its event was written.**
  The request after a compaction is `[system, summary, pins, kept tail, later
  events]`: the summary and the pins come first, the tail that was kept verbatim
  follows, and the last message is always the newest event — never an old pin —
  and the message right after the system one is a `user` message (M3 review B1).
  A `compacted` event whose `through_seq` is at the very end of the log renders
  last, after nothing.
- **A compaction keeps a tail verbatim** (`compact_keep`, default 4 messages) so
  the model still sees what it was doing; `fold` cannot cut between an assistant
  tool call and its result, so the cut moves back to that assistant when a
  `tool_settled` would otherwise start the tail.
- **A later compaction supersedes every earlier one whose range it covers** — a
  later `through_seq` at or past the earlier one's, or past the earlier
  `compacted` event itself (its kept tail is what put it there) — so one summary
  stands and each pin renders once (M3 review B2). Its transcript is the *fold*
  of the range, so the earlier summary is part of what the second one
  summarises; the new `through_seq` never reaches back before an earlier
  compaction's range.
- **An automatic compaction is refused when it cannot shrink the context** (M3
  review N1): if even an empty summary would leave the fold at least as large,
  no summarising request is sent, the automatic trigger stays quiet until the
  context grows past the size it refused at, and the refusal is one
  `error {where: "compact", retryable: false}` event (the only log event that
  can carry a decision). An explicit `compact` (human or agent) is always
  honoured: `compacted.tokens_after >= tokens_before` is visible in the event,
  and the UI says so.
- **A queued `compact` survives a resume** (M3 review N2): a `compact` call whose
  `tool_settled` says `compaction queued` with no `compacted` event after it is
  re-queued by the resume, with the focus the call carried.
- **The auto trigger never fires before the log's first step**, and only when a
  plan exists: at the first `before_step` the "conversation" is the task alone,
  and compacting it would cost a provider call to save nothing.
- **The summarising request is sealed**: the `compact_prompt` as the system
  message and one user message (the focus line, then the transcript of the
  range), built by the same client — so it carries the session id and can hit
  the parent's cache — with **no tools**. Verbs are read while it is in flight,
  exactly as during a step (M3 review S2): `stop` ends the turn without a
  `compacted` event (the half-summary is dropped), `end` closes the session, and
  anything else waits for its boundary.
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
  final line that is a whole event without its newline is kept (the newline is
  added). A torn final line — anything shorter — is copied to
  `events.jsonl.torn` and **cut off** when the log is opened, so the log holds
  whole events only: no unparseable line for `jq` or the TUI, and never two
  lines with the same `seq` (M3 review S3). The resume that finds it reports it
  once as `error {where: "log"}`, after the cut, so a later resume reports
  nothing, and damage a previous resume already reported is not reported again.
- **The TUI no longer double-prints tool lines.** The session printed
  `→ bash … · ok · N B` on stdout while the TUI painted its own line for the
  same event; that write is now headless-only (`!json && !tui`), so the painter
  stays the only writer to the terminal.

## M4

- **Every bash call starts under `kelir bg-shim` — the milestone's "always start
  via the shim" option.** The shim is the call's process owner, so promotion is
  "stop waiting" plus a marker file: no running child is ever re-parented and no
  work is destroyed by a clock. The consequence that shapes the rest: the shim
  hands the command the **output file itself** (`Stdio::from(File)`), so the
  command writes its output even after the session is gone, and the session only
  *tails* that file for the inline result (`commands/<dir>/out`, same path, same
  `Capture`, the same tool result as M0's pipes). A pipe the session read would
  block the job once the session died.
- **`bg/<id>.status` is the job's whole state, written only by its shim.** The
  spec's `state`, `pid`, `exit_code`, `started`, `finished` plus the fields the
  machinery needs: `shim` (the shim's pid: the process group on Unix, the process
  whose job holds the tree on Windows), `cmd`, `out`, `kind` (`job`|`call`),
  `promoted`, `call` (the provider call id that started it), `job` (the Windows
  job object's name) and `error` (why the command could not start). States are
  `running`, `exited`, `cancelled`.
- **The status file is replaced atomically and read defensively.** A write goes
  to `<id>.status.<pid>.tmp` and then `rename`s over the real name (which
  replaces it on Windows too), so a reader sees the old file or the new one.
  `Status::read` returns `None` for a missing, empty or unparseable file, and
  `bg ls`/the watcher skip it (and skip `.tmp` leftovers): a torn status can
  never break a session or the CLI.
- **The job id of a promoted call is the call's output directory name**
  (`<seq>-<step>-<n>`), not a fresh `bg_<ulid>`: the model already has it in the
  `promoted` result, and `bg_started.out_path` and `tool_settled.out_path` name
  the same file. An explicit `kelir bg run` gets `bg_<ulid>`. Job object names
  are `kelir-bg-<session id>-<call dir>`, unique per call, so `bg cancel` can
  open one by name from another process.
- **`bg_started.call_id` is the provider call id** of the bash call that started
  the job (the shim exports `KELIR_CALL_ID`), present for a promoted call and for
  a `kelir bg run` made from inside a call; absent for a job started by hand.
- **Where jobs live: `KELIR_SESSION_DIR/bg` inside a session, else
  `<state>/bg`.** The shim exports `KELIR_SESSION_DIR`, `KELIR_CALL_ID`,
  `KELIR_BIN` (this binary, so `kelir bg run` works in a call even when kelir is
  not on PATH) and the inherited `KELIR_SOCK`. `kelir bg out|wait|cancel <id>`
  also search the sessions' `bg/` directories, so they work from a bare shell
  after kelir exited.
- **`kelir bg` reaches the session through `KELIR_SOCK`, and the session polls as
  a safety net.** `kelir bg run` sends one `bg` request (the status file as
  `args`, `from: "job"`) to the address in `KELIR_SOCK`; the session records
  `bg_started` at its next boundary (at once when idle). Independently the
  session watches `bg/*.status` every 250 ms, so a report the socket never
  carried (a session that was busy, a dead socket, a status written by a previous
  process) still reaches the log. Both paths record through the same `note_bg`,
  which dedupes against the log: `bg_started` once per id, `bg_settled` once per
  id.
- **The `bg` verb is internal but logged like every other verb** (`name: "bg"`,
  `args` = the status, `from: "job"`, `boundary: "now"`). It applies at the first
  boundary it can get and never between a tool call and its result; a malformed
  report is refused (`bg: not a job status: …`), not half-applied. A report from
  anyone but `job` is refused as well (`bg: an internal verb; jobs report
  themselves`), so a `ctl` client cannot fabricate a job in the log.
- **The wake is `bg_settled` plus a `user` event with `from: "job"`**, written at
  the next boundary (or immediately when the session is idle, which starts a
  turn). The message is the spec's intent: `[bg job <id> exited 0 after 3.0s]`,
  the command line, the output path with byte/line counts, the last 1 KB and how
  to read the rest. **The fold renders the `user` message and not the `bg_`
  events**, so a job is reported exactly once and the live and resumed paths
  build the same request. `bg_settled` carries one field the spec's table does
  not list, `state` (`exited`|`cancelled`), so a reader need not infer it from
  `exit_code`.
- **`resume` folds `bg/*.status` into the log** after the unknown-result block:
  for every job or promoted call, `bg_started` if the log lacks it, and
  `bg_settled` + the wake message if it settled while nothing was running. A
  wake found this way sets `has_task`, so a bare `kelir resume <id>` runs the turn
  that reads it — "a job that finishes while the session is not running is only
  recorded" holds on the shim's side (nothing wakes a dead session), and the
  record is what the resume then acts on. A job still running is only recorded,
  and the resumed session's watcher wakes it when it settles.
- **Cancellation is a kill, then a status write, never a cooperative verb.**
  `kelir bg cancel` kills the tree (Unix `killpg` of the shim's group; Windows
  `TerminateJobObject` of the job, plus the shim's own process for a `bg run`
  job), waits up to 2 s for the shim to be gone and only then writes
  `state: "cancelled"`. The shim is the only writer while it lives, so the two
  can never interleave; if the job finished on its own first, that is reported
  and nothing is overwritten.
- **Windows: the call's Job Object is not `KILL_ON_JOB_CLOSE`, and the shim
  watches its owner instead.** The spec asks that a detached job not be in a
  kill-on-close job; a promoted call's tree is still the call's job object, kept
  alive by the shim's own handle (the shim opens it by name, assigns the
  suspended command to it and holds it until it exits), so closing the session's
  handle does not take it down. What protects the other direction — a session
  that dies **without** promoting — is the shim: it waits on the session's
  process handle (Windows) or compares `getppid()` (Unix) and, within one 25 ms
  poll, writes `cancelled` and takes the tree down. That also closes M3's
  documented gap on Unix, where a `SIGKILL`ed session left a running call behind:
  `tests/e2e_m4.rs` proves on both platforms that nothing keeps writing after the
  session is killed. `CREATE_BREAKAWAY_FROM_JOB` is tried for a `kelir bg run`
  shim and dropped only if an enclosing job forbids breakaway (the job then runs
  inside whatever job the caller had).
- **Windows: the process clears the inherit flag on its own stdin/stdout/stderr**
  (`close_handle_inheritance`). A background job was holding the caller's pipe
  open — `kelir run --json | jq` stayed open for as long as a promoted job ran,
  measured 6 s for a `sleep 6 &` — because `CreateProcess` inherits every
  inheritable handle. Clearing `HANDLE_FLAG_INHERIT` at startup changes nothing a
  child needs (std makes the handles it passes to a child inheritable per spawn).
- **`promote_after` is `config.toml`'s `promote_after` (seconds, default 600),
  overridden by `KELIR_PROMOTE_AFTER` (`0` disables promotion).** The timer takes
  the elapsed time as an argument (`bg::promotion_due`), so it is unit-tested
  without a clock. A promoted call settles with the spec's `promoted` outcome; its
  result names the job id, `kelir bg out|wait|cancel` and the output so far. The
  `bg_started` event follows that `tool_settled`, which is when the job exists.
- **`kelir bg-shim` is a hidden subcommand of the same binary**, given everything
  it needs on its command line (`--dir --id --out --cmd --shell --shell-arg
  --kind --owner --call --job --sock`), so a shim is self-contained and
  detachable: `setsid` on Unix (the spec's word, via `pre_exec`),
  `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB` on
  Windows. A call's shim is instead grouped by the session (`process_group(0)` /
  `CREATE_SUSPENDED` + Job Object) so that `stop` can reach it.
- **A call's kill is now "kill the shim's group/job"** (Unix `killpg` on the
  shim's group, Windows `TerminateJobObject`), and a task cancelled mid-call
  still kills its tree (a `Drop` guard in the tool). A leftover `cmd &` child
  keeps running, as documented for M0: the group/job is still held by the
  session, so it dies when the session does.
