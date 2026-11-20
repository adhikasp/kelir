# The event log (`events.jsonl`)

**Summary:** every event type in a session's append-only log, the fields a
reader (a gate, the TUI, `jq`, the next resume) can count on, and what writes it.
**Read this when:** you are asserting on `events.jsonl` or NDJSON stdout, writing
a gate or a metric, or adding an event (`src/event.rs`: `EventBody`, `Payload`).

One JSON object per line, `{"seq": n, "ts": "<RFC 3339>", "type": …, …}`. `seq`
is monotonic from 1 and each batch is fsync'd. A torn last line is skipped on
read and reported on resume; the fragment is kept in `events.jsonl.torn`.

**Context is always `fold(events.jsonl)`**, the same code path live and on
resume: `src/fold.rs` renders the frozen system message, then `user`,
`assistant`, `tool_settled.result` and `source` events in `seq` order, with
`compacted` replacing its range.

## Types

<!-- generated:events -->
| type | fields | written by |
|---|---|---|
| `session_started` | `id`, `parent?`, `depth`, `cwd`, `os`, `kelir_version`, `provider`, `model`, `system{text,hash}`, `sources[]`, `tools[]`, `reasoning_effort?`, `key_origin?` | the first event of a session: the frozen request prefix |
| `user` | `text`, `from`, `delivery`, `attachments[]` | a human message: the TUI, `kelir ctl say`, a job's wake-up, a channel |
| `source` | `kind`, `name`, `text?`, `path?` | context that arrived after the start: a `hint`, a skill, an instruction or memory edit, an `attachment` or `attachment-note`, a channel message |
| `step_started` | `step`, `provider`, `model`, `request_hash` | just before one provider request |
| `assistant` | `step`, `text`, `reasoning?`, `tool_calls[]`, `finish_reason?` | the model's answer to one step |
| `usage` | `step`, `prompt_tokens`, `completion_tokens`, `cached_tokens`, `cache_write_tokens`, `cost`, `cost_source?`, `allowance?`, `kind?` | the final stream chunk; `cost_source` says whether `cost` is the provider's own (`provider`), tokens times a known price (`table`) or not known (`unknown`, cost 0.0); `kind: "compact"` marks the summarising request and `kind: "hint"` the hint scorer's own call |
| `warning` | `kind`, `message`, `value`, `threshold` | the session when the last prompt passes `warn_prompt_tokens` or the cost passes `warn_cost`, once per crossing; never rendered into the conversation |
| `verb` | `name`, `args`, `from`, `boundary`, `error?` | every verb received, applied or not (`boundary: "rejected"` for a refusal) |
| `tool_started` | `call_id`, `name`, `input`, `pid?`, `dir?` | intent: written before the tool's process starts |
| `tool_settled` | `call_id`, `outcome`, `exit_code?`, `bytes`, `lines`, `out_path`, `spilled`, `result` | the call's one commit; `result` is what the model sees |
| `turn_ended` | `outcome`, `reason?`, `final_text` | the end of a turn; `reason` distinguishes `steps` from `provider` pausing |
| `mcp_started` | `mcp_id`, `server`, `tool`, `args`, `parent_call_id?` | a `kelir mcp <server> <tool>` call reached the session broker with the session token (intent: written before the server is asked); `args` is the arguments' shape (keys kept, each string replaced by `<string, N bytes>`), never their values |
| `mcp_settled` | `mcp_id`, `server`, `tool`, `outcome`, `exit_code`, `bytes`, `out_path`, `parent_call_id?` | the call's one commit; `outcome` is `ok`, `error`, `timeout`, `unavailable`, `disabled`, or `unknown` (a resume found no settlement), and the result is in the file `out_path` names |
| `bg_started` | `bg_id`, `call_id?`, `cmd`, `pid?`, `out_path`, `pubkey?` | a promoted bash call or an explicit `kelir bg run` |
| `bg_settled` | `bg_id`, `call_id?`, `cmd`, `exit_code?`, `state`, `out_path` | a job that settled while this session owned its log (`state`: `exited` or `cancelled`) |
| `gate` | `gate`, `event`, `decision`, `reason`, `ms`, `rewritten?`, `error?` | every gate run, including the ones that failed open (`error` says why: crash, timeout, garbage) |
| `compacted` | `through_seq`, `summary`, `pinned_seqs[]`, `by`, `tokens_before`, `tokens_after`, `cache_restart` | a compaction; the fold replaces everything up to `through_seq` except the pins |
| `resumed` | `from_seq`, `unsettled[]` | a session reopened from its log |
| `error` | `where`, `message`, `retryable`, `status?`, `attempt?`, `retry_in_ms?` | provider errors, broken sources, refused verbs; never a response body and never a key |
| `session_ended` | `reason` | the end of the session |
<!-- /generated -->

## Payloads over 10 KB are never inline

A string over `PAYLOAD_LIMIT` (10 KB) is written to `blobs/<seq>-<field>` in the
session dir and the event holds
`{"spilled": true, "path": …, "bytes": N, "preview": …}`. `fold` reads it back,
so the request bytes are the same as if it had been inline. Tool *output* takes a
different route: the whole output is in `commands/<call-id>/out` from the first
byte, and `tool_settled.result` is the ≤10 KB head/tail the model sees
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
