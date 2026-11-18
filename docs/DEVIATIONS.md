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

