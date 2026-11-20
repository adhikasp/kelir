# Deviations from `docs/SPEC.md`

Small, deliberate differences, with the reason. Everything else tracks the spec.

## Session-socket hardening (STEER-7, 2026-11-19)

The brief is `docs/tasks/STEER-7.md`; the trust boundary lives in `src/trust.rs`, the
permissions in `src/perm.rs`, and the two are wired in `src/ipc.rs` (the socket) and
`src/session.rs` (where a refusal becomes an event). Where the code had to choose:

- **The token is checked at the listener, and a refusal is not logged.** An unauthenticated
  request is answered with a clear error and its connection closed *before* it reaches the
  session's channel, so it cannot write a `verb` event (the brief asks for a `verb` event on a
  refusal, which is the right record for a caller that *is* this session's — a foreign or
  missing job id — and the wrong thing to give a stranger: it would be a log-write primitive for
  anyone who finds the pipe). Both kinds of refusal look the same to the caller: an error, no
  state change.
- **The token rides in an environment variable, never in an argv.** The brief suggests
  `KELIR_TOKEN` for tool children, which is what happens; the bg shim also needs it to report
  itself, and an argv is world-readable (`ps`, `wmic process`) while an environment is not, so
  the shim is started with `Command::env` and the `--token` flag was removed rather than kept for
  symmetry. The Windows pipe and the Unix socket are also the client's route to `KELIR_TOKEN`
  for a process that only inherited `KELIR_SOCK`: `ipc::request_value` fills it in.
- **`<session dir>/token` is owner-only, and the session checks it again at start.** On Unix
  that is `0600`; on Windows a *protected* DACL with one `FILE_ALL_ACCESS` ACE for this user,
  written with `SetNamedSecurityInfoW` **by name** (opening the file with `WRITE_DAC` and passing
  the handle gave `ERROR_ACCESS_DENIED` for a file this process had just created — the handle has
  to be opened with `WRITE_DAC`, which is not what a plain create returns). `trust::owner_only`
  reads the result back (`stat` mode on Unix, `GetNamedSecurityInfoW` on Windows) and the session
  refuses to start on a token file others can read; `perm.rs` has a Windows test that an
  inherited-ACL file reads as too wide and a restricted one as owner-only.
- **`cancel` is a verb with its own row, not a `bg` report.** `kelir ctl <session> cancel <id>`
  sent `{"id": id}` before it parsed (there was no `cancel` arm in `verbs::cli_args`, so the
  positional id was dropped and the request refused) — it is now a first-class verb, applied at
  once, and it is an *action*: `Scheduler::take_where` leaves actions in the queue where
  `take_turn_messages` used to take everything, and the idle loop applies a pending `cancel`
  itself, so a `cancel` against an idle `--keep` session kills the job instead of waiting for a
  boundary that never comes.
- **A job id is `owns`-checked against three things**: the in-memory registry (what this process
  started), `bg/<id>.status` in this session's own `bg/`, and `commands/<id>/` (a promoted
  call's id is its output dir name). The `bg/` directory is created at session start, so an
  empty session has an empty registry rather than a missing directory.
- **Reachability, not identity, is what the token proves.** Documented rather than defended
  against (see `docs/SPEC.md`, *Who may drive a session*): a process that can read this session's
  environment or token file runs as the user, in the session, and is trusted. The token stops a
  different session's child, a stray script and a pipe found by name.
- **`cancel` writes `bg/<id>.cancel` while it works, and readers honour it for 30 s.** A cancel
  kills the shim first and writes the `cancelled` status last (up to `CANCEL_WAIT`, 2 s), so in
  between the status file says `running` and the shim behind it is dead — which every reader is
  built to report as `lost`, and which the session's own watcher did: the e2e test that cancels a
  job got a `bg_settled` of `lost` roughly half the time, and so would a real session. Two
  changes close it: the marker file (`bg::observed_in`) tells readers a cancel is in flight, and
  `Session::note_bg` re-reads the status file when it records a report instead of trusting the
  watcher's snapshot, which may predate the cancel. The marker expires
  (`CANCEL_MARKER_TTL`), because a cancel killed mid-flight would otherwise leave a dead job
  `running` for ever, and is removed on every way out of `cancel_at`, including refusals.

## Retrying provider errors (STEER-5, 2026-11-19)

The brief is `docs/tasks/STEER-5.md`; the policy lives in `src/retry.rs` and the
wait itself in `session::stream`. Where this differs from the brief:

- **Attempts are bounded by a waiting budget, plus a finite stand-in for
  "unlimited".** The brief says attempts are unlimited inside `retry_max_wait`.
  A provider that answers instantly with 429 would then spin: the budget is
  spent in time, and a zero-wait loop never spends any. So `retry::MAX_ATTEMPTS`
  (60) caps the count regardless, `retry_max_attempts` in `config.toml` caps it
  further, and `retry_max_wait = 0` (unlimited waiting) does **not** mean
  unlimited attempts. `retry_max_attempts = 0` is read as unset, not as "never
  retry" (a person who wants no retries sets `retry_max_attempts = 1`). With the
  default schedule the waits reach the 120 s cap after seven attempts, so the
  budget — not the count — is what ends a normal outage.
- **A provider hint can never shorten a retry below a floor.** `Retry-After: 0`,
  a reset header of `0s` and a date already past are all well-formed answers that
  mean "now"; taken literally, a gateway that answers that way for ever got a
  request every few milliseconds (the review measured 309/s). The floor is
  `min(250 ms, retry_base_ms)`, so a person who configured a fast test schedule
  keeps it and the default schedule never waits less than a quarter second
  (STEER-5 review S2). An absurd *large* hint is still honoured, and still
  pauses the turn when it does not fit the budget.
- **A `Retry-After` longer than what the budget has left pauses at once.** The
  brief says to honour the header; waiting 4 s of a 1 s budget and *then* failing
  would honour neither. The provider's hint is clamped to the budget, and a hint
  that does not fit pauses the turn immediately.
- **There is one `paused` outcome with a `reason`.** STEER-4's step cap and
  STEER-5's exhausted retry budget are the same event with two causes: the
  event carries the shared `final_text` payload with the message "provider
  unavailable after N attempts / M: <reason>; say `continue` to retry" and a
  `reason` of `provider`; the step cap's `reason` is `steps` and its message is
  "turn paused after N steps; say continue to resume". A one-shot `run` exits 0
  for either, a `--keep` session stays up with a fresh step count and a fresh
  budget, Herdr gets `blocked` for both (the one word this Herdr shows for
  "waiting for a human" — the STEER-4 rule, now the rule for every pause), and
  the TUI's status word is `paused`. `resume::interrupted` treats `paused` like
  `stopped`, so a resume (or a later `say`) retries from the same fold.
- **A queued `before_step` verb applies between two attempts; a `now` verb cuts
  the wait short.** The brief asks that `model` during a wait "uses the new
  model", and a retry is a fresh request: so an ordinary wait that runs out
  applies the `before_step` boundary before re-sending, which also lets a queued
  `say` join the fold and `end` take effect. A verb whose answer is `applies:
  "now"` (`say --now`, `model`) does not wait for that boundary: it is applied
  the moment it arrives, the wait ends, and the request goes out at once —
  rebuilt from the new fold or the new model, with a fresh budget and the new
  provider's governor. A person's correction must not sit out a `Retry-After`
  that can be a week long (STEER-5 review B2). The `verb` event still records
  which boundary it applied at. The ack is answered *after* the verb lands, so
  `say --now` and `model` during a wait answer `applies: "now"` with the state
  the next request really carries — for `model`, `state.model` is the new one
  (STEER-5 review N1).
- **`later` keeps its boundary even during a wait.** The brief lists `later`
  among the verbs that "take effect immediately", and it does — as a *delivery*:
  it is accepted, logged and queued the moment it arrives. What it does not do
  is cut the wait: `later` asks for the turn end, its ack says `applies:
  "turn_end"`, and the text joins the next turn from the fold, which is what
  SPEC and the `verb` boundary have said since M1. Cutting a `Retry-After` for
  a message that must not be sent mid-turn would put the message into a request
  the sender asked it to stay out of (STEER-5 review N3).
- **A signal is a `stop`, but only while a wait is running.** Ctrl-C (and
  SIGTERM on Unix, Ctrl-Break on Windows) during a retry or slot wait ends the
  turn `stopped` with the reason `interrupted (Ctrl-C)` and leaves a `--keep`
  session up, instead of killing a process whose log says nothing. The listener
  is installed **once per process**, before the session is announced
  (`signals::install` at the top of `session::drive`), because Tokio never
  restores a signal's default disposition once a `Signal` exists; a Ctrl-C
  during a request in flight or while a tool runs therefore also reaches the
  listener, which keeps it pending rather than killing the process. A **second**
  interrupt within two seconds exits (`exit 130`), which is the way out of a
  wedged session — the rule the TUI states. The pending state is a
  **level-triggered flag**, not an edge-triggered wakeup: it stays set until a
  wait acts on it and is cleared there, so a signal that arrives before the wait
  begins is not lost (STEER-5 fixes round 2; the first version compared a
  `watch` baseline, and `watch::Sender::send` drops its value when nothing is
  subscribed — the interrupt vanished in the gap between the `error` event and
  the wait's first poll, which failed about two runs in six under load).
  Test coverage: `src/signals.rs` has unit tests of the state machine, including
  200 rounds of "signal, then wait"; the Unix half is driven end to end by real
  `SIGINT`s to real sessions
  (`tests/e2e_steer5.rs::an_interrupt_during_a_wait_ends_the_turn_stopped`,
  which signals a running wait, and `::an_interrupt_before_the_wait_begins_is_not_lost`,
  which signals *before* the wait exists — it fails on the old mechanism with
  "the interrupt was lost; the turn never ended"). The Windows half is those
  unit tests only: no test may *send* a console event, because
  `GenerateConsoleCtrlEvent` addresses a whole console and not one process
  (AGENTS.md hard rule). This is the one remaining honest gap: no automated test
  presses Ctrl-C on a Windows console.
- **A retryable provider error's message is the status and its class, never the
  body.** For an outage the `error` event's `message` is `http 503: provider
  error` or `http 429: rate limited`: it is a wait, nothing in the body helps a
  person or a script decide anything, and a gateway that quotes the request (or
  a 4xx that prints the key) would otherwise write a credential into
  `events.jsonl` and into NDJSON stdout (STEER-5 review S3; this file used to
  accept the body because it "never leaves the machine", which is not true of
  stdout). A *refusal* (400/401/403/404/422) still carries its body — "unknown
  model" is the answer that lets a human fix the invocation — bounded to 500
  characters and with the session's own key replaced by `[redacted]`.
- **NDJSON carries the wait as the `error` event, not as a separate line.** The
  visible line ("rate limited (429): retrying in 34 s, attempt 5, say stop to
  cancel") goes to the TUI's scrollback and status line and to stderr in human
  mode; under `--json` stdout stays one event per line, and the wait *is* the
  `error` event with `retryable`, `status`, `attempt` and `retry_in_ms`.
- **The concurrency governor counts what is in flight, not what is configured.**
  A slot is held for exactly one request: `[providers.<name>]
  max_concurrent_requests = N` takes an advisory OS lock on
  `<state root>/locks/<base-url-host>-<slot>` for the duration of one
  `POST /chat/completions`, polling every 100 ms, with the status line saying
  "waiting for a provider slot (N)". Two config names pointing at one endpoint
  share the endpoint's slots, because that is what the provider's limit is per.
  `0`/unset is off. Linux and macOS use `flock`, Windows `LockFileEx` (not
  `File::try_lock`, which needs Rust 1.89 and the MSRV is 1.88); both are
  released by the kernel when the holder dies, which is the property a pid file
  does not have.
- **The governor's slot wait is not an `error` event.** It is kelir waiting for
  its own sibling, not a provider failure, so it is a visible line and a status
  line only. It is interruptible like a retry wait: `stop`, `end` and a signal
  end the turn, `say --now` and `model` apply at once and simply look for the
  slot again.
- **The governor follows the provider a request goes to.**
  `max_concurrent_requests` is read from the config entry the `model` verb
  resolved to, every time, so a swap to a governed provider starts taking its
  slots and a swap away stops being held to the old limit (STEER-5 review S1).
  A compaction's summary is a provider request on the same account and takes a
  slot like any step.
- **A retry hint prefers the `Date` header and the tightest reset window.**
  Absolute hints (an HTTP-date, an epoch, an RFC 3339 stamp) are measured from
  the response's own `Date` when it has one, so a skewed local clock cannot turn
  a 90 s hint into none at all (review N1). When a response carries several
  reset headers (`x-ratelimit-reset-requests` and friends) the *shortest* still
  closed window wins: a provider reports every window it counts, and the one
  that fired is the tight one, not the daily one (review N2).
- **A compaction summary is tried once, never retried.** A 429 on the
  summarising request skips that compaction — the context stays as it was and
  the next step runs, with a `where: "compact"` error in the log — rather than
  spending the turn's waiting budget on a summary nobody asked for. A step
  always gets the budget (review N3; M3 review N4 made the same call).

## STEER-6: the retro's loop changes (2026-11-19)

- **On Windows a command always runs from a file; on Unix a long one does.** The spec's shell
  row is `bash -c` (or `cmd.exe /C`), i.e. the command as the last argument. Two things break
  that on Windows:
  - a command line past ~8,191 characters is silently **cut** by Git for Windows' `bash.exe`,
    and a cut heredoc usually exits 0 (three M5-A calls, one STEER-4 call, each with a
    half-written file);
  - every run of backslashes is **halved** on the way to the shell (the Win32 command line, then
    msys' argv rewriting), at any length: `printf 'a\nb'` prints a newline, a regex or a Windows
    path means something else by the time the shell sees it (STEER-6 review S1, reproduced on the
    baseline binary). A length threshold cannot fix that one, because the mangling happens below
    it.

  So the rule is one route per platform: on Windows every command is written to
  `<call dir>/cmd.sh` (`cmd.bat` for `cmd.exe`, `cmd.ps1` for PowerShell) and the shell is handed
  the path; on Unix a short command keeps `-c` (no msys layer, one fewer write) and a command
  over `SHELL_CMD_MAX = 6000` bytes goes through the same file route. The Windows route is not a
  length threshold, so a command's meaning cannot change at a boundary; the file lives in the
  call's own directory, not a temp dir, so the exact bytes that ran stay readable, which is what
  makes a cut diagnosable. `kelir bg run` writes `bg/<id>.sh` for the same reason (one directory
  holds every job, so the name carries the id).
- **The shell keeps the switch that makes it read a file.** `cmd.exe` gets `/C <path>` and
  PowerShell `-File <path>`, because `cmd.exe <path>` starts an interactive shell on a null stdin
  that runs nothing and exits 0 (STEER-6 review B1: a long command reported success and never
  ran), and `-Command <path>` would run the path as PowerShell source. A POSIX shell reads the
  script from its own argv, so its `-c` goes — and a login-shell spelling (`bash -lc`, `-ic`,
  `-ec`) keeps the flags before the `c` and drops the `c` (`bash -l cmd.sh`), so that common config
  takes the file route instead of silently keeping the halving argument route (STEER-6 review N5).
  Any argument before the switch (`cmd.exe /D /C`) is kept. A shell kelir does not know (`--eval`)
  is left exactly as configured on Unix; on Windows a command for such a shell is an **error**
  naming the argv and the spellings that work, because the only route left there is the one that
  halves backslashes.
  Verified with the inbox `powershell.exe` in an e2e test (`-File cmd.ps1`, output written by the
  script). A Store-installed `pwsh` cannot be used as a call shell at all on Windows: it is an
  AppX execution alias, and Windows refuses to put the process an alias starts into a Job Object
  (`cannot assign it to the job: Access is denied`, os error 5). That is the app-packaging model,
  not a kelir bug (the STEER-6 reviewer hit the same wall probing `pwsh`); use the inbox
  `powershell.exe`, or `bash`, as `shell`.
- **A script's line endings are normalised, and only those.** A POSIX shell gets LF: a CRLF
  `cmd.sh` puts a `\r` at the end of every line and breaks a heredoc terminator outright.
  `cmd.exe` and PowerShell get CRLF. The normalisation follows the **shell family, not the route**:
  a short POSIX command on the argument route is normalised the same way, because otherwise the
  same text would mean two things at the 6,000-byte boundary (`echo a > f<CR>` created `f<CR>`
  below it and `f` above it — STEER-6 review N10). A lone `\r` inside a payload (how a heredoc
  writes a literal CR to a file) is untouched. `KELIR_SHELL_RAW=cr` sends the bytes exactly as
  written on both POSIX routes, for a caller that needs the raw bytes. A `.bat` also starts with
  `@echo off`: without it `cmd.exe` echoes the prompt and the command line into the result, so a
  20 KB command came back as 20 KB of echo (STEER-6 review N6).
- **A long command also stays off the shim's own command line.** `kelir bg-shim` used to take
  the command as `--cmd <text>`, and Windows refuses to *start* a process whose command line is
  past ~32 KB (`os error 206`) — so a 40 KB heredoc failed before the shim existed. It travels
  as `--cmd-file <path>` instead. `kelir bg run <cmd>` still takes the command as an argument,
  so a job command past ~32 KB cannot be started through the CLI at all; the model writes the
  script with `edit` and runs that. This is a process-argv ceiling, not a kelir choice.
- **`edit` takes `edits: [{old, new, replace_all?}, …]` as well as `old`/`new`.** The spec's
  tool row names `{path, old, new, replace_all?}`. `edits` is the same operation repeated: it is
  atomic (all hunks checked before anything is written, one write at the end) and applied in
  order against the evolving text, because the dogfood model used `python -` patch scripts for
  multi-hunk changes and those fail with exit 0. `old`/`new` are unchanged and the tool list is
  the same, so the frozen request prefix only moves at a release.
- **`session_started.reasoning_effort` is a field the spec's table does not list.** The spec's
  provider section leaves the effort field unverified and has no way to record it. The value
  kelir sends is recorded (and absent when the config says `none`) so the log and the request
  agree and a cache-key question can be answered from the log alone. Measured on the OpenCode Go
  gateway: the flat `reasoning_effort` field is honoured (the reasoning tokens move with it, ×2
  between `low` and `high` on a reasoning-heavy prompt); the OpenRouter-style
  `{"reasoning": {"effort": …}}` object is accepted but added latency with no reason-token
  effect, so kelir sends the flat field on both providers. A model with a long thinking phase
  may still spend its budget before the answer (one probe returned an empty `content` with
  `finish_reason: length`); the knob is a request, not a guarantee.
- **`compact_at_tokens` is a third auto-compaction input.** The spec says 80 % of the window
  (configurable). A 1M-token window at 80 % is 800K, and a real session passes 150K with 76 % of
  the context being old file reads. Compaction now fires at
  `min(window * auto_compact_at, compact_at_tokens)`; the default target is 150,000 and `0`
  disables the cap, so the window fraction alone decides, exactly as before.
- **`parallel_tool_calls` is asked for when the model is known to take it — and the answer does
  not depend on a warm cache.** The spec says parallel calls are allowed; the request said
  nothing. OpenRouter's `supported_parameters` states the capability per model, and that wins when
  the list has an entry. But the list is metadata fetched with a bound, so it can be missing
  (first session on a machine, an unreachable endpoint) or stale; the loop then fell back to *not*
  asking, which made F4's benefit silently depend on machine state (STEER-6 review S2: live, a
  480-model list through a proxy took > 1.5 s, the cache was stored empty, and the request had no
  field although the model advertises it). The rule now:
  - the provider's list entry, when there is one (both directions: `false` is believed);
  - else a conservative built-in family table (`deepseek/`, `anthropic/`, `openai/`, `gpt-4`,
    `gpt-5`, `google/`, `gemini-`, `qwen/`, `kimi`, …);
  - else `false`: an unknown model on an unknown gateway is not asked.
  A slow `/models` fetch is also no longer recorded as a failure (only a real error is), so a
  proxy that answers in 2 s does not leave the cache empty for an hour. `parallel_tool_calls` is
  still only sent to OpenRouter: Go's list states no such capability, and a family name is not
  evidence about *that* gateway's acceptance.
- **`reasoning_effort` is gated on what the provider advertises, with one retry on a 400.** The
  field used to go to every model on every provider; a model or gateway that refuses it turned
  every step into a hard error until the person found the config key (STEER-6 review S3). Now:
  - listed and advertising `reasoning_effort` (or the `reasoning` object form it is shorthand
    for) -> sent;
  - listed without it -> not sent, even when the config names one: the provider said no;
  - the list has the model but no capability list at all (OpenCode Go) -> unknown, so the
    default still goes (measured as honoured on Go, see above);
  - the list has no entry for the model -> only when the person set `reasoning_effort`
    explicitly. The default `medium` is kelir's own choice and does not go to a model nobody
    vouched for.
  An HTTP 400 whose body names the field is retried **once** without it, logged as an `error`
  note, and remembered: no later request in the session carries it. The retry does not spend the
  provider-attempt budget (a 400 is not transient). `session_started.reasoning_effort` records
  what is actually sent, so the log and the request agree.
- **The "your command may have been cut" notice needs evidence.** The spill notice fired on any
  output that contained one of the shell's truncation markers, including a command that merely
  printed them: `git grep "unexpected EOF" src` matched `spill.rs` itself, and the model was told
  a command that ran fine may have been cut (STEER-6 review N1). It now fires when a marker is in
  a shell's own diagnostic line (`bash: warning: here-document at line 1 delimited by
  end-of-file …`, printed *before* the body it did read), or the command failed, or (for a
  successful command) the marker is in the last three lines. A successful search that quotes the
  markers in its middle stays quiet.
- **The base prompt is pinned byte-for-byte.** It is embedded at build time and is the head of
  every request's frozen prefix, so a changed byte moves the cache for every session.
  `tests/fixtures/prompt.md.snapshot` holds the released bytes and a unit test compares them;
  rewording the prompt means updating the snapshot deliberately in the same commit (the rule
  list in `sources.rs` still pins the lines a retro put there).

## Sources and skills (M5-A, 2026-11-19)

- **A source edited mid-session is found by a per-boundary content hash, not a watcher.**
  The brief allows either ("a watcher or a per-turn mtime/hash check", "say why in
  `DEVIATIONS.md` if you deviate"). kelir re-reads the recorded sources at each
  `before_step` and compares a hash of the body. A watcher would be a thread, an OS
  notification subscription or an open handle on a process that is already hosting
  someone's desktop, and the recorded set is a handful of small files — one base prompt,
  a few instruction files, one file per skill. The hash is also *exact*: an `mtime` that
  changed while the body did not is not news. `src/sources.rs::Watch` holds that state
  and a resumed session rebuilds it from the log's `sources[]` **and** its `source`
  events, so it reports exactly the edits a live session would have and no repeats
  (M5-A fixes F8).
- **The watcher remembers one hash per source: the one the context currently carries.**
  Not "every hash it ever carried" (M5-A fixes F1). A source edited A→B→A is announced
  as A again, because the model's latest word on the file before that is B, and a rule
  that is "no longer on disk" must not be the last thing the model was told. The same
  holds after a `compact` folds the B message into a summary: the folded message is no
  longer in the context, so the next boundary says what the file holds now even when its
  bytes did not change. The `skills` verb follows the same rule against the last
  `source` event of that name (F1b).
- **One hashing rule for every reader: the whole file.** The render, the watcher and the
  `skills` verb all read a source through `sources::read_source` — UTF-8 BOM dropped,
  invalid UTF-8 decoded lossily and noted, size capped with a visible truncation notice —
  and hash exactly those bytes. A skill's `source` message therefore carries its front
  matter too (so its `description` arrives) and a skill added by `skills --path` is not
  announced a second time by the watcher (F3, N5).
- **`sources[]` gains a `kind`.** The spec's `{name, path, hash}` cannot tell a skill
  directory called `notes.md` from an instruction file, so the log records what the
  source was *where it was found*: `prompt`, `instruction` or `skill`. A resume reads it
  back instead of guessing from the name (F9); a log written before this fix has no
  `kind` and is read back from the name as before.
- **The embedded base prompt records an empty `path`, not the string `embedded`.** A
  sentinel is a path a reader (or the watcher) will try to open: any file named
  `embedded` in the cwd became a bogus `source` message. `SourcePath` is a typed
  `Embedded | File(PathBuf)` in the code, the log's empty string says the same, and a
  config `prompt = "file"` records that file's real path (F2, F2b). The base prompt's
  own `sources[]` entry also carries `kind: "prompt"`; its `source` events keep the kind
  `instruction` (the spec's list has no `prompt`).
- **Skills are recorded in `session_started.sources[]`.** The spec says `sources[]`
  carries `{name, path, hash}` "for everything that went in", and a skill does go in — as
  a *pull* target. Recording it is what lets a skill edited mid-session arrive as a
  `source` message, and its hash is of the `SKILL.md` as read from disk (front matter
  included). No skill body is rendered into the system prompt: the prompt names the
  directories and nothing else, exactly as the pull rule requires.
- **The base prompt's own edit arrives as `kind: "instruction"`.** The spec's `source`
  kinds (`hint`, `skill`, `instruction`, `memory`, `attachment`, `channel`) have no
  `prompt`, so the base prompt uses `instruction` and is told apart by its name,
  `prompt.md`.
- **`KELIR_HOME` overrides the home directory** (`paths::home`, after which `HOME` and
  `%USERPROFILE%` follow as before). Sources are discovered in `~/.claude/skills`, so a
  test run on a developer's machine would otherwise depend on that machine's skills; the
  test harness points `KELIR_HOME` at its own temp dir. It is the same kind of environment
  override as `KELIR_STATE_DIR`/`KELIR_CONFIG_DIR`, and nothing else reads it.
- **The test harness scrubs every `KELIR_*` variable before every spawn** (and
  `TERM_PROGRAM`/`HC_TERMINAL_ID`, the pane markers), *before* the test's own environment
  goes on. kelir dogfoods itself, so a suite run from inside a kelir session inherited
  that session's own job directory and socket: `bg out` read the *session's* jobs, and a
  test could report (or cancel) a job in the session running it. `KELIR_PROMOTE_AFTER`,
  `KELIR_HERDR`, `KELIR_CRASH_AT` and `KELIR_TEST_PANIC` were the same hazard: an
  ambient value in a developer shell silently changed what the suite exercised (M5-A
  fixes N2). This is test infrastructure, not shipped behaviour, but it is recorded here
  because it changes what the suite proves about process isolation.
- **Instruction discovery walks up until the repository root or the home directory.**
  The spec says "walking up from the cwd"; the brief for M5-A says "stops at the repo root
  or home" (N3). Every `AGENTS.md`/`CLAUDE.md` found on the way is kept, nearest first —
  a monorepo's root rules and a subdirectory's rules are both in the prompt (F4) — but a
  file planted in `/tmp` or above the home directory is not.
- **Sources are capped and every cap is stated.** One instruction file or skill body is
  read up to 64 KiB, all instruction files together up to 256 KiB; going over cuts the
  text on a char boundary and appends `[kelir: truncated; N bytes on disk, the first M
  shown]`, and the log gets one `error` with `where: "source"` per file (F7). A file is
  skipped rather than half-read when it does not fit the *total* budget, so the watcher
  re-reads it the same way and cannot announce it again. A non-UTF-8 file is read lossily
  with the same kind of note (F5), a `SKILL.md` without valid front matter is skipped
  with its reason (F6), and a tracked source deleted mid-session is one note, not a
  `source` event and not silence (N4).

## Models metadata comes from the provider (2026-11-19)

The built-in window table is gone. It assumed 128k for every `deepseek` model, so the 1M-token
model kelir now defaults to would have been compacted at a tenth of its real window (the maintainer,
2026-11-19). `src/models.rs` parses `GET {base}/models` from both providers into one normalised
list (`context`, `tools`), caches it at `<state>/models-<provider>.json` for 24 hours, and backs off
for an hour after a failed fetch so an offline box does not pay the timeout on every start. The
fetch happens at session start (and never mid-turn, including on a `model` swap, which resolves
against the list already cached) and is bounded to 1.5 s, so a start with a stale cache can cost up
to 1.5 s once and a start with a fresh cache costs nothing.

`[model_windows]` and `window` still win over the provider; a model the list does not name uses
`DEFAULT_WINDOW` (128 000). Every verb's `state` carries `window` and `window_source`
(`config`, `api`, `cache`, `default`), and the TUI banner shows it, so which source answered is
visible without reading the code. The test mock now serves `/models` (canned, never recorded, never
consuming a scripted reply) so a session can start in a test.

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
- **A `max_steps` cap pauses the turn; it never fails it (STEER-4).** Unset or
  `0` in `config.toml` (or `--max-steps 0`) means no cap at all, which is the
  default: the old `100` default is gone, and `0` is accepted as "unlimited"
  rather than refused, so a stopgap config cannot break a session. Reaching an
  explicit cap ends the turn with `turn_ended outcome=paused` (not `error`), the
  NDJSON `result` says `paused` with exit code 0 (a pause is not a failure), and
  stderr/TUI print "turn paused after N steps; say continue to resume". A
  `--keep` session stays up and its next `say` opens a new turn with a fresh
  count; `resume` of a paused log continues it like a stopped one (see
  `resume::interrupted`). The TUI status line shows `step used/limit` only when a
  cap is set, and the TUI banner names the cap; the phase word is `paused`. A
  verb's answer already carries a `state` object; it now also reports
  `turn_steps` (the turn's own count) and `max_steps` (the cap, `null` when
  there is none), so a steering human sees a pause coming. Two details the
  STEER-4 review asked for (S1, N1, N2): the cap is checked *before* a step is
  counted, so after a pause `state.turn_steps` is the number of steps that ran
  (never `cap + 1`), and the count is right the moment the pause is reported;
  with a cap set the TUI's status line says `step 0/N` while idle too, not a
  bare `step 0` that would hide the limit until a turn started; and a paused
  turn reports Herdr `blocked`, not `idle`, because a paused turn *is* waiting
  for a human (saying `continue`) and a pane supervisor cannot otherwise tell
  it from one that never started. `paused` would be the clearer word but this
  Herdr does not know it (measured with `scripts/live/m2_osc_probe.py` on `hc`
  0.1.21: `state=paused` and `state=waiting` leave the pane's status `null`,
  `state=blocked` shows as `blocked`), so the record keeps a word Herdr shows.
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
  `step` and `turn` continue from the log instead of restarting. The turn's own
  step count restarts at 0 (it is per turn), and today's `max_steps`/`--max-steps`
  applies to the resumed turn like every other config knob.
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
  whose job holds the tree on Windows), `shim_started` and `pid_started` (when
  the shim and the command really started, read from the OS by the shim: the
  only thing a reader will signal a pid on), `cmd`, `out`, `kind` (`job`|`call`),
  `promoted`, `call` (the provider call id that started it), `job` (the Windows
  job object's name) and `error` (why the command could not start). States are
  `running`, `exited`, `cancelled` — and `lost`, which a *reader* reports for a
  `running` status whose shim is gone (it is never written to the file).
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
  themselves`), so a `ctl` client cannot fabricate a job in the log. The one
  exception is `bg-spawn` (a `kelir bg run` asking its session to start the shim
  outside the call's Job Object, see below): it is answered and not logged, and
  the status file the shim writes is the record, exactly as for any job. A
  routed job still reports itself to the session like any other (`bg run`
  notifies after it reads the status), because the watcher's 250 ms poll would
  otherwise put `bg_started` in the log after `turn_ended` of the call that
  asked for it.
- **The wake is `bg_settled` plus a `user` event with `from: "job"`**, written at
  the next boundary (or immediately when the session is idle, which starts a
  turn). The message is the spec's intent: `[bg job <id> exited 0 after 3.0s]`,
  the command line, the output path with byte/line counts, the last 1 KB and how
  to read the rest. **The fold renders the `user` message and not the `bg_`
  events**, so a job is reported exactly once and the live and resumed paths
  build the same request. `bg_settled` carries one field the spec's table does
  not list, `state` (`exited`|`cancelled`|`lost`), so a reader need not infer it
  from `exit_code`. A wake that follows a promoted `tool_settled` is written
  after the whole batch's results, never between them: a call's results must stay
  contiguous for the next request (M4 review N5).
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
  and nothing is overwritten. **Nothing is signalled on the strength of a pid in
  a status file.** The shim records its **own** start time, read from the OS in
  its own process (`src/bg.rs` `Status::shim_started`, `proc::start_time`), and
  `cancel` compares that exact value with the OS's answer for the pid, within one
  second (M4 review round 2, G1). The job's `started` is not used at all: a job
  can run for hours before its shim is lost, and a pid is reused within seconds,
  so the old `started` ± 120 s window let a recycled pid pass for the shim and
  be killed. A status that records no start time, a pid that now belongs to a
  process that started at another time, and a platform that cannot report start
  times (macOS) are all refused rather than guessed, and `bg ls`/`bg wait` show
  such a status as `lost`, not `running`.
- **A lost job's tree is still reachable, and `cancel` says exactly what it
  cannot do** (M4 review round 2, G2). A job object's *name* dies with its last
  handle, so a killed shim left the tree with `cancel` printing "nothing to
  cancel" while the tree kept running. Two things fix that:
  - the shim **lends its job handle to the command** (`Job::lend_to`,
    `DuplicateHandle` into the command's process) when the job is born
    (`kelir bg run`) or promoted — never before, so an unpromoted call's job
    stays kill-on-close. The command is in the tree and outlives a killed shim
    in the ordinary case, so the job keeps its name and `TerminateJobObject`
    still reaches every process in it, msys's exec'd grandchildren included;
  - when no handle is left, `cancel` falls back to what the status proves: a
    tree walk from the command's own pid (`proc::terminate_tree`, parent links,
    deepest first, and a child older than its parent is not walked into), with
    the command's `pid_started` compared exactly as above. On Unix it uses the
    shim's process group: a pid number cannot be reused while a group with that
    number still has a member, so a non-empty group is that job's tree, while a
    live process on the shim's pid that is not the shim is refused.
  When none of that can be proved — no recorded pid or start time, a recycled
  pid, a platform that cannot list the group, or a command that left its
  children behind and died — `cancel` prints the reason and signals nothing
  (exit 1), and `bg ls` still shows the job `lost`.
- **Windows: the call's Job Object forbids breakaway and is `KILL_ON_JOB_CLOSE`
  until promotion.** M4's first cut allowed breakaway so a `kelir bg run` inside
  a call would not be trapped; an msys `spawn` asks for
  `CREATE_BREAKAWAY_FROM_JOB` whenever the job allows it, so exec'd
  grandchildren (`sleep.exe`, a nested `bash.exe`) left the call's tree and
  neither `cancel` nor a killed session took them down (M4 review F1). The flag
  is gone: the whole tree stays in the job, which is what makes "cancel kills
  the job's process tree" true on Windows. A `kelir bg run` made inside a call
  now asks the session to start its shim (the internal `bg-spawn` request, the
  same `from: "job"` trust as `bg`): the session is outside the call's job, so
  an explicit job still outlives the session. `KILL_ON_JOB_CLOSE` is back for an
  unpromoted call's job (M4 review F6): a session that is killed outright still
  has its job handles closed by the kernel, and a child a finished call left
  behind (`cmd &`) goes with them; a promotion clears the flag before the
  session closes its handle (`ProcessGroup::detach`), and the shim's own handle
  plus its owner watch keep a promoted tree alive. What protects the other
  direction — a session that dies **without** promoting — is the shim: it waits
  on the session's process handle (Windows) or compares `getppid()` (Unix) and,
  within one 25 ms poll, writes `cancelled` and takes the tree down. That also
  closes M3's documented gap on Unix, where a `SIGKILL`ed session left a running
  call behind: `tests/e2e_m4.rs` proves on both platforms that nothing keeps
  writing after the session is killed.
- **Unix: a finished call's shim stays as its tree's owner while the group is
  not empty** (`proc::group_others`, a `/proc` scan on Linux). Without it a
  `cmd &` child of a *finished* call outlived a `SIGKILL`ed session, because the
  session's `killpg` only runs on a graceful exit (M4 review F6). The shim exits
  the instant the group is empty — the common case is unchanged, and a call
  settles on its *status file*, not on the shim process. The owner check stays
  at 25 ms (`getppid()`, one cheap syscall, so a killed session is noticed at
  once) while the `/proc` group count **backs off** from 25 ms to 1 s
  (`LINGER_MAX`) as the lingering child lives on (M4 review round 2, G4): the
  scan was ~23 % of a core per lingering shim, and it is what a dev server
  started with `&` paid for hours. Measured on the Linux box with the release
  binary and one 60 s child in the group (`/proc/<shim>/stat` utime+stime over
  10 s, 100 Hz): **before 224–225 ticks = 22.4–22.5 % of one core; after 8–9
  ticks = 0.8–0.9 %**, with the "a killed session takes the leftover tree"
  guarantee unchanged (`tests/e2e_m4.rs`, `tests/review_m4.rs` R18/R19 and the
  leftover-child tests still pass there). The one platform gap: where `/proc`
  cannot be read (macOS) `group_others` answers `None` and the shim does not
  linger, so that half of the guarantee holds only on a graceful exit there;
  macOS is compile-only today.
- **`kelir bg out --follow` reads only what is new** (seek to the offset, read to
  EOF), so following a job that writes a lot is not quadratic (M4 review N1).
- **A job whose shim is gone is reported `lost`, and the status file is left
  alone** (M4 review F4). `running` is what the shim wrote; a reader
  (`bg ls`, `bg wait`, the session's watcher, `resume`) turns it into `lost` when
  the shim is provably gone — or when its identity cannot be proved — with
  `error` saying so, so a `taskkill /F /IM kelir.exe`, an OOM kill or a reboot
  can never leave a job that runs for ever. Readers never write another
  process's status; `cancel` on a lost job reaches the tree as described above
  and otherwise says why it cannot. A *call* whose shim dies before its status
  settles now returns `outcome: "unknown"` with its partial output instead of
  hanging, and its remaining tree is stopped.
- **`kelir bg run` keeps the argument boundaries the caller wrote** (M4 review
  F3): one word is a command line as written (shell syntax allowed), several
  words are literal arguments, each quoted (`printf '[%s]' 'a b' c`). Shell
  syntax belongs in one quoted word, which is how `bash -c` and every other
  command line already work.
- **`--cwd` (and a resumed session's recorded cwd) is the directory a call and a
  job run in** (M4 review F2): the shim is spawned in it and the command inherits
  it, on every path (`bash`, and `kelir bg run` both where the caller was and
  through `bg-spawn`).
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
  `CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP`, plus
  `CREATE_BREAKAWAY_FROM_JOB` when the caller's job allows it, on Windows. A
  caller whose job forbids breakaway — a call's job, by design, or a harness's —
  gets the shim without the breakaway flag, which is still outside the *call's*
  job when the session starts it on request (`bg-spawn`). A call's shim is
  instead grouped by the session (`process_group(0)` / `CREATE_SUSPENDED` + Job
  Object) so that `stop` can reach it; a session that has **no console of its
  own** gives that shim `CREATE_NO_WINDOW` too (`win::has_console`), so the call
  never has to allocate a console either.
- **Windows: the job's shim is `CREATE_NO_WINDOW`, not `DETACHED_PROCESS`**
  (M4 review round 2, G3). A detached shim has no console, so its
  console-subsystem child (`bash.exe`) makes one: a **visible console window per
  job** on someone's desktop, and, once hundreds of those accumulate, a start-up
  that stalls for minutes — measured on the dev box as 160 `bg run "echo hi"`
  jobs whose shims never got past spawning `bash`, all with a console process of
  their own. With `CREATE_NO_WINDOW` the shim gets a console with no window and
  the command inherits it. It is still outside the session's console and, with
  breakaway, outside its Job Object, so the job outlives the session exactly as
  before (`tests/e2e_m4.rs`: a promoted tree survives a killed session; a `bg
  run` made in a call survives it; a job started by a console-less CLI still
  runs, and so does a `bash` call of a console-less session). Numbers on the dev
  box, 20 concurrent `kelir bg run "echo hi"` from a console-less (DETACHED)
  parent, timed until every status said `exited`: `DETACHED_PROCESS` 1.9 / 3.3 /
  2.7 s wall, `CREATE_NO_WINDOW` 0.6 / 0.7 / 0.7 s. On a healthy box both finish
  (20/20); the stall is the degraded state the incident left behind, and it does
  not reproduce on demand, so the test asserts the guarantee (every job exits,
  nothing left behind) rather than a timing.
- **A call's kill is now "kill the shim's group/job"** (Unix `killpg` on the
  shim's group, Windows `TerminateJobObject`), and a task cancelled mid-call
  still kills its tree (a `Drop` guard in the tool). A leftover `cmd &` child
  keeps running, as documented for M0: the group/job is held by the session and
  by a lingering shim, so it dies when the session does.

## Two providers, and OpenCode Go's key from opencode itself (STEER-2, 2026-11-19)

The brief is `docs/tasks/STEER-2.md`; the code is `src/credentials.rs` and `src/doctor.rs`. Where
this differs from the spec text, or needed a call the brief left open:

- **`kind` is gone from the provider tables.** The brief says a `[providers.<name>]` table only
  overrides settings of the two providers, and the name decides the provider. A `kind = "…"` key in
  one of those tables is therefore ignored rather than honoured: `[providers.opencode-go] kind =
  "openrouter"` still configures `opencode-go`. It is the same rule as a literal `key` — a config
  that names a provider a second way is a config that can disagree with itself.
- **A literal `key` is warned about, not fatal.** A config file with `key = "…"` (top level or in a
  provider table) loads, prints one warning to stderr naming the two supported sources, and never
  uses the value. Failing the load would make an existing config unusable for the sake of a warning
  it does not need, and a maintainer's old config must keep starting.
- **`key_env` is a fallback, not a replacement.** SPEC's first-match list is `OPENROUTER_API_KEY`,
  then `key_env`, then `key_cmd`; the code briefly let `key_env` replace the default name, which a
  migrated config with `key_env = "KELIR_OPENROUTER_API_KEY"` would have turned into "missing API
  key" on a machine where only `OPENROUTER_API_KEY` is set. Fixed for both providers: the default
  variable is tried first, the named one second (Go's default is `KELIR_GO_API_KEY`, which keeps the
  override semantics above).
- **An expired store entry is read twice, not refused on the first read.** `parse_auth_export` now
  parses a stale entry and `store_credential` decides: expired-or-inside-the-margin means one quiet
  250 ms pause and one re-read (opencode rotates its token as a side effect of running, so the first
  read can race that write); if it is still expired, that is the usage error naming the expiry and
  `REFRESH_FIX`. Never a third read, never a request with a token that cannot work.
- **The `[providers.<name>]` migration error names the table and the file.** `config::load` records
  the files it merged and reports `` `[providers.kelir]` in <path>: … rename the table to
  `[providers.openrouter]` or `[providers.opencode-go]` (`kind = …` is no longer used)``. The
  `provider = "<name>"` / `--provider` error stays the short one that names the pair.
- **`kelir doctor` exits 1 only for a provider that matters.** The selected provider failing, or any
  provider whose credential is *expired*, is a failure; a provider with no key that this session does
  not use is reported as information (`JSON ok` is true). A machine that runs Go deliberately has no
  `OPENROUTER_API_KEY`, and `doctor` should not call that a broken install.
- **A helper's stdout is read up to 64 MiB.** Past the bound the read stops, the child is killed
  (it is blocked on a pipe nobody drains) and the error says the helper printed more than the bound.
  64 MiB rather than 1 MiB because a real opencode export on the dev box was measured at 30 MB with
  padding, and the bound is a memory guard, not a size expectation.
- **The store read is synchronous, and it cannot overlap the model-list fetch.** The brief asks for
  the read to run concurrently with other startup work. It cannot run beside `GET {base}/models`:
  that fetch authenticates with the *same* key (Go's list wants it), so the key has to exist first.
  What could overlap is the config load, the cwd check and the system render, which together cost
  microseconds. So `credentials::resolve` blocks for at most `AUTH_TIMEOUT` (5 s), called from
  `prepare`/`reopen` and from a `model` verb that swaps provider, before the first request is built
  (a background task holding the key would put the secret in another place). Measured on the dev
  box: `opencode auth export opencode` 1221 ms cold, 289/308/296 ms after; the whole `kelir ask` on
  `opencode-go` 1964/2063 ms through the store vs 1661/1483 ms with `KELIR_GO_API_KEY` set, i.e.
  the read costs ~0.4 s of a cold start that is dominated by the first network round trip.
- **`opencode auth export opencode` is run twice at most, and never in a loop.** When the store's
  entry is expired or inside the two-minute margin the read is repeated once, because the CLI
  refreshes a token as a side effect of running; if the second read is no fresher, an expired entry
  is the usage error naming its expiry and a nearly-expired one is used (the 401 path is then one
  more read and one retry). There is no third read and no polling.
- **The 401/403 re-read is one per request, not one per turn.** `reread` is reset when the request
  is rebuilt for a new reason (a `model` swap, a `say --now`), so a turn cannot loop on it: the
  second 401 in a row is the answer and ends the step as a refusal.
- **An expired store credential is a usage error (exit 2) before any request.** The brief says to
  fail with the expiry time and the fix; failing *before* the request, rather than after a 401, is
  what keeps a token that cannot work off the wire at all.
- **`key_cmd` is named by its file name in messages and in `key_origin`.** A `key_cmd =
  ["C:/Users/someone/bin/getkey"]` would otherwise put a private path into `session_started` and
  into `kelir doctor`'s output; the repo's public-log rule and the brief's "no private paths" both
  apply. Only the program's own name is recorded.
- **The Go provider's `key_env` stays `KELIR_GO_API_KEY` in the config entry.** It is what the
  precedence list starts with *when set*; the store is the default that answers when it is not.
  `config::load` therefore stays pure (no process read, so `kelir doctor` and a unit test can load a
  config without starting a helper) and the resolution happens exactly once, at session start.
- **The session socket's trust boundary is the token *and* a per-job capability.** SPEC said a
  request must carry the session token and that the session answers `bg`/`cancel`/`bg-spawn` only
  for jobs "it started or registered", where a `bg/<id>.status` file or a `commands/<id>/`
  directory counted as registration. That is not a boundary: every bash child holds the token by
  design (it is exported as `KELIR_TOKEN`), and it can create either path itself, which is exactly
  how 43 fabricated "bg job was lost" messages reached a turn (M5-A retro F8, STEER-7 review B1).
  Now the session is the only issuer of job ids (random, `bg_`/`call_` + 128 bits), the registry is
  this process's memory rebuilt from the log, files are never evidence, and a `bg` report must also
  carry the job's own 256-bit capability (`KELIR_JOB_CAP`, handed to that job's shim only). A call's
  id is registered with an empty capability, so no report is ever accepted for it.
- **What the boundary does not defend against, precisely.** A sibling process running as the same
  user can read `<session dir>/token` (and, in principle, `/proc/<pid>/environ`). That is the
  same-user trust boundary and it is deliberate: the token and the capability protect against
  other users, against a stray process or a script that found the pipe by name, and against *the
  model's own tools impersonating a job's shim* — not against code the user chose to run with the
  user's own rights. The capability is what makes the second half work: a tool child has the token,
  so the token can only say "somebody inside this session"; the capability says "that job's shim".
  The token file is still written (`kelir ctl` needs it) and now removed when the session ends.
- **A resume adopts jobs from the log only, and an adopted job has no capability.** The registry is
  memory, so a resumed session rebuilds it from `bg_started` and from a new optional
  `tool_started.dir` field (the call's own `commands/` name). An adopted job answers `cancel` but
  never a `bg` report: the secret died with the process that minted it, and a shim that outlives
  its session settles through its status file and the watcher instead. A status file left behind by
  a job nobody logged is therefore still listed by `kelir bg ls` (which reads files) but never
  becomes an event.
- **`tool_started` grew an optional `dir`, and request lines are bounded.** A `tool_started` written
  by an older kelir has no `dir` and is read as a call whose job id a resume does not adopt (its
  `commands/` dir is still there; a `cancel` for it is refused after a resume, which is the safe
  side). The listener reads at most 64 KB and waits at most 10 s for a request line, so an
  unauthenticated peer cannot make the session hold a megabyte or a connection for as long as it
  likes (STEER-7 review N3).
