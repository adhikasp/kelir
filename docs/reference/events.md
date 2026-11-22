# The event log (`events.jsonl`)

**Summary:** every event type in a session's append-only log, the fields a
reader (a gate, the TUI, `jq`, the next resume) can count on, and what writes it.
**Read this when:** you are asserting on `events.jsonl` or NDJSON stdout, writing
a gate or a metric, or adding an event (`src/event.rs`: `EventBody`, `Payload`).

One JSON object per line, `{"seq": n, "ts": "<RFC 3339>", "type": …, …}`. `seq`
is monotonic from 1 and each batch is fsync'd. A torn last line is skipped on
read and reported on resume; the fragment is kept in `events.jsonl.torn`.

This is **format 1**. The first event carries `format: 1` and `session_started`;
a reader refuses a log written in any other format with "written by a different
kelir format; start a new session" rather than guessing at renamed fields. There
is no compatibility reader: a log from a different version is not read.

Format 1 has 16 event types. Calls of every kind share `call_started` /
`call_settled`, distinguished by top-level `kind` (`tool`, `mcp`, `bg`, `child`);
per-call details live in `info`. Usage from a provider request that has no
assistant event (compaction, a gate-scoring call) is a `step_usage` event.
Threshold warnings and notification outcomes are both `note`, distinguished by
`note.kind`.

**Context is always `fold(events.jsonl)`**, the same code path live and on
resume: `src/fold.rs` renders the frozen system message, then `user`,
`assistant`, `call_settled.result` and `source` events in `seq` order, with
`compacted` replacing its range.

## Types

| type | fields | written by |
|---|---|---|
| `session_started` | `format: 1`, `id`, `parent?`, `depth`, `cwd`, `os`, `kelir_version`, `provider`, `model`, `system{text,hash}`, `sources[]`, `tools[]`, `reasoning_effort?`, `key_origin?` | the first event of a session: the frozen request prefix |
| `user` | `text`, `from`, `delivery`, `attachments[]` | a human message: the TUI, `kelir ctl say`, a job's wake-up, a channel |
| `source` | `kind`, `name`, `text?`, `path?` | context that arrived after the start: a `hint`, a skill, an instruction or memory edit, an `attachment` or `attachment-note`, a channel message, and for named sessions and jobs a `clock` (the new date on a later day), a `job` (the one status line) or a `carry` (what a replaced session left) |
| `step_started` | `step`, `provider`, `model`, `request_hash` | just before one provider request |
| `assistant` | `step`, `text`, `reasoning?`, `tool_calls[]`, `finish_reason?`, `usage?` | the model's answer to one step; `usage` is the final stream chunk's token counts (`prompt_tokens`, `completion_tokens`, `cached_tokens`, `cache_write_tokens`, `allowance?`, `kind?`) |
| `step_usage` | `step`, `prompt_tokens`, `completion_tokens`, `cached_tokens`, `cache_write_tokens`, `allowance?`, `kind?` | a provider request with no assistant event: `kind: "compact"` for summarising, `kind: "hint"` for the gate scorer's own call |
| `verb` | `name`, `args`, `from`, `boundary`, `error?` | every verb received, applied or not (`boundary: "rejected"` for a refusal) |
| `turn_ended` | `outcome`, `reason?`, `final_text` | the end of a turn; `reason` distinguishes `steps` from `provider` pausing |
| `call_started` | `id`, `kind` (`tool`, `mcp`, `bg`, `child`), `info{name, input?, dir?, session?, parent?, depth?, background?, model?}` | intent for a tool, MCP, background or child call, written before it starts; absent `info` fields are not serialised. MCP `input` is the arguments' shape (keys kept, string values replaced by `<string, N bytes>`), never their values |
| `call_settled` | `id`, `kind`, `outcome`, `exit_code?`, `bytes?`, `lines?`, `out_path?`, `spilled?`, `result?`, `state?` | the call's one commit; `outcome` includes `ok`, `error`, `stopped`, `skipped`, `denied`, `disabled`, `promoted`, `timeout`, `unavailable`, `unknown`, `done`, `paused`, `killed`, `lost`; `result` is what the model sees; MCP and bg calls record their whole output in the file `out_path` names |
| `gate` | `gate`, `event`, `decision`, `reason`, `ms`, `rewritten?`, `error?` | every gate run, including the ones that failed open (`error` says why: crash, timeout, garbage) |
| `note` | `kind`, `text` | a threshold crossing (`prompt_tokens`) or a notification hook outcome (`notify`, text names the hook event); never part of the conversation the model reads |
| `compacted` | `through_seq`, `summary`, `pinned_seqs[]`, `by`, `tokens_before`, `tokens_after`, `cache_restart` | a compaction; the fold replaces everything up to `through_seq` except the pins |
| `session_ended` | `reason` | the end of the session |
| `session_title` | `title` | the first human prompt, as a deterministic one-line label (no model call) |

## Payloads over 10 KB are never inline

A string over `PAYLOAD_LIMIT` (10 KB) is written to `blobs/<seq>-<field>` in the
session dir and the event holds
`{"spilled": true, "path": …, "bytes": N, "preview": …}`. `fold` reads it back,
so the request bytes are the same as if it had been inline. Tool *output* takes a
different route: the whole output is in `calls/<call-id>/out` from the first
byte, and a `call_settled` result is the ≤10 KB head/tail the model sees
(`docs/reference/files.md`).

## The session id is also the cache key

`session_started`'s `id` is what the two providers see as the session/prompt-cache
key. It never changes for the life of a session, which is why a `model` verb that
swaps provider starts a new cache line but not a new log.

## Testing against the log

- NDJSON on stdout (`kelir run --json`) is the same events, one line each, with
  a `result` line at the end.
- An e2e test asserts on the log, never on a rendered screen: the shape above is
  the contract.
- Prefer `--json`'s `result` for the exit status (`done`, `stopped`, `paused` with
  a reason) and the log for everything else.
