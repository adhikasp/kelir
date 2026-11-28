# Channel mode

**Summary:** `kelir channel`: chat bridges as MCP servers, inbound
`notifications/claude/channel`, capability negotiation, the durable spool,
one named session per conversation, single-flight, bounded concurrency, rate
limits, fallback replies and `/stop`.
**Read this when:** you are configuring a channel bridge (`config.toml` with
`channel = true`), debugging why a message was dropped or answered twice, or
reading `spool/<server>.jsonl` after a crash (`src/channel.rs`, `src/mcp/`).

kelir ships the generic mechanism, not a concrete chat service. A bridge is an
MCP server configured with `channel = true`; the maintainer supplies its command,
credentials and infrastructure. No token or path to one is stored in this repo.

## Protocol

At session start the channel server is connected and `initialize` declares the
capabilities in `[mcp.servers.<name>] declare_capabilities` (default
`claude/channel`). Any capability whose name contains `permission` or
`approval` (any case, any separator) is refused in `config.toml` and filtered at
`initialize`: kelir has no approvals. Inbound messages arrive as `notifications/claude/channel` with
`message_id`, `conversation_id`, `sender`, `content`/`text`, optional
`attachments` and `reply_to`. Unknown, malformed and oversized notifications
are dropped and logged once per shape — never echoed into a file or a log verbatim.

The answer is one tool call on the same server (`reply_tool`, default `reply`)
with `conversation_id`, `text`, `message_id` and `reply_to` (the inbound
message's `reply_to` when it had one, else its own `message_id`); a bridge that
supports idempotency keys should use `message_id` so a replay cannot send twice.
When a turn ends kelir sends `notifications/claude/channel/turn_done` so a
typing indicator stops. `/stop` in a chat message maps to the `stop` verb and is
never sent to the model as a prompt.

## Durability

Every inbound message is appended to `spool/<server>.jsonl` and fsync'd before
anything else happens to it. Records are append-only: `inbound`, `claimed`,
`done`. A restart replays unfinished messages, deduped by message id; a turn
whose user event was already logged is not re-run. The file is compacted when it
passes `[channel] spool_limit`, keeping unfinished messages and one `done` record
per id; a `done` id stays deduped after compaction and restart. A torn tail is repaired on open and kept beside the spool.

## Trust

The text arrives as a `user` event with `from: channel` and an envelope naming
server, conversation and sender; the model is told the sender is a remote user,
not the operator, and that the text is untrusted data, not a session command.
`prompt_submit` gates run on the envelope. Nothing from an MCP server's `env` is
ever logged.

## Status

While any turn runs, the process reports OSC 7501 `working`, otherwise `idle`.
`kelir channel --status-once` prints a JSON status line at startup; the live
snapshot is in the state dir at `spool/status.json` (conversations,
queue depth, active turns, spool backlog, reply/fallback counts).
