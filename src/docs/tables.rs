//! The tables `docs/reference/` is generated from: verbs, config keys and
//! event types, each with the sentence its docs row carries.
//!
//! These are *data*, not prose: the prose lives in `docs/reference/*.md` and a
//! marker says where the table goes. A drift test ([`crate::docs::tests`])
//! reads the source and fails when code and table disagree in either
//! direction, so a new verb, key or event cannot ship undocumented.

/// One verb: how it is called, when it applies, and what it writes.
#[derive(Debug, Clone, Copy)]
pub struct VerbDoc {
    /// The name `kelir ctl <session> <name>` and `verbs::parse` accept.
    pub name: &'static str,
    /// The arguments, as a person types them.
    pub args: &'static str,
    /// The `verb` event's `boundary`: when it takes effect.
    pub applies: &'static str,
    /// What it does, including the event it writes.
    pub effect: &'static str,
}

/// Every verb [`crate::verbs::parse`] accepts, in the spec's order.
pub const VERBS: &[VerbDoc] = &[
    VerbDoc {
        name: "say",
        args: "`<text>` or a JSON object with `text`; `--now`",
        applies: "`now` (with `--now`) or `queued` (the next boundary)",
        effect: "injects a user message (`user` event, `delivery` `now`/`next`); \
                 `--now` stops the running calls first",
    },
    VerbDoc {
        name: "later",
        args: "`<text>` or a JSON object with `text`",
        applies: "`turn_end`",
        effect: "queues a user message for the end of the turn (`user` event, `delivery` `later`)",
    },
    VerbDoc {
        name: "stop",
        args: "`[reason]`; `--call <id>`",
        applies: "`now`",
        effect: "stops the turn: every running tool process tree is signalled and its call \
                 settles `stopped`; `--call` stops one call and the turn continues",
    },
    VerbDoc {
        name: "model",
        args: "`<provider>/<model>` or `<model>`",
        applies: "`next_step`",
        effect: "swaps provider/model for the rest of the session; the next `step_started` \
                 carries the new one",
    },
    VerbDoc {
        name: "tools",
        args: "`--disable <name>` / `--enable <name>` (repeatable)",
        applies: "`next_step`",
        effect: "disables or enables a native tool, or MCP calls (`mcp:<server>` or \
                 `mcp:<server>.<tool>`); the tool list sent to the provider never changes, a \
                 disabled tool's call settles `disabled`",
    },
    VerbDoc {
        name: "skills",
        args: "`--name <n> --path <file-or-dir>` or `--text <body>`; `--kind skill|instruction`",
        applies: "`next_step`",
        effect: "adds or replaces one source; it arrives as a `source` message at the tail, never \
                 as a rewrite of the frozen system prompt",
    },
    VerbDoc {
        name: "compact",
        args: "`[focus]`",
        applies: "`next_step`",
        effect: "summarises the earlier context at the next boundary and writes a `compacted` \
                 event; the task and the messages a human typed stay verbatim",
    },
    VerbDoc {
        name: "view",
        args: "`<path>`",
        // The `verb` event says `now`; the attachment itself lands at the next
        // boundary, like the message it belongs to.
        applies: "`now`",
        effect: "queues an image for the next message: a `source` event of kind `attachment` at \
                 the tail, so the cached prefix is untouched; an audio or video file is stored \
                 under the session and noted (kind `attachment-note`), not sent",
    },
    VerbDoc {
        name: "bg",
        args: "one job status object, as `kelir bg run` sends it",
        applies: "`now`",
        effect: "a job reported itself or changed state; the session writes `bg_started`/\
                 `bg_settled` and, for a settled job, the wake-up `user` message",
    },
    VerbDoc {
        name: "cancel",
        args: "`<job id>`",
        applies: "`now`",
        effect: "kills one of this session's background jobs and records it `cancelled`; a job id                  the session did not start is refused",
    },
    VerbDoc {
        name: "status",
        args: "none; `--json` prints the answer as one JSON line",
        applies: "`now`",
        effect: "read-only and never logged: answers at once, even mid-request or in a retry wait,                  with steps used/limit, turn, last prompt size and context %, cache hit ratio,                  tokens, cost and its source, last error, retry in progress, running calls,                  background jobs, children, and `mcp` (one `{name,state,tools,channel}` per configured MCP server; the state word only, never config or server output). `context.window` is `null` (with `context.window_source` `default`) until the model list answers or a window is configured, and `percent` stays `null` with it. A provider-reported cost of exactly 0 on a priced model is replaced by a table estimate (`cost.source` `table`). Needs the session token like every verb",
    },
    VerbDoc {
        name: "end",
        args: "none",
        applies: "`now`",
        effect: "ends the session (`session_ended`); running calls are stopped first",
    },
];

/// One `config.toml` key (top-level, or a `[providers.<name>]` field).
#[derive(Debug, Clone, Copy)]
pub struct KeyDoc {
    /// Where it lives in the file: `` (top level) or `providers.*`.
    pub scope: KeyScope,
    /// The key as written in `config.toml`.
    pub name: &'static str,
    /// The Rust type the TOML value must deserialise to.
    pub kind: &'static str,
    /// What happens when the key is absent.
    pub default: &'static str,
    /// A CLI flag or environment variable that overrides it, if any.
    pub overrides: &'static str,
    /// What it does.
    pub effect: &'static str,
    /// `fn` in `src/config.rs` whose reading gives the field its line.
    pub field_fn: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyScope {
    Top,
    Provider,
}

impl KeyScope {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyScope::Top => "",
            KeyScope::Provider => "providers.*",
        }
    }
}

/// Every key `RawConfig` (scope ``) and `RawProvider` (scope `providers.*`)
/// has, in the order the reference reads them.
pub const CONFIG_KEYS: &[KeyDoc] = &[
    KeyDoc {
        scope: KeyScope::Top,
        name: "provider",
        kind: "string",
        default: "`openrouter`",
        overrides: "`--provider`",
        effect: "which of the two providers this session uses",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "model",
        kind: "string",
        default: "`deepseek/deepseek-v4.1-flash-1017`",
        overrides: "`--model`",
        effect: "the model id sent with every request",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "shell",
        kind: "string or array of strings",
        default: "Windows: Git for Windows `bash.exe`, else `bash` on PATH, else `cmd.exe /C`; elsewhere: `bash` on PATH, else `$SHELL`, else `sh`, each with `-c`; the array is the argv prefix",
        overrides: "—",
        effect: "the program `bash` tool calls and `kelir bg run` commands run in",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "spill_limit",
        kind: "integer (bytes)",
        default: "`10240`",
        overrides: "—",
        effect: "output over this many bytes is written to `commands/<call>/out` and the model \
                 sees a spill notice instead of the whole text",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "max_steps",
        kind: "integer",
        default: "unset: no cap. `0` also means no cap",
        overrides: "`--max-steps`",
        effect: "cap the steps of one turn; reaching it *pauses* the turn (`turn_ended` with \
                 `reason: \"steps\"`), and the next message starts a new turn",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "cache_ttl",
        kind: "string",
        default: "unset: the provider's own default",
        overrides: "—",
        effect: "the `cache_control.ttl` sent with the request (`5m`, `1h`), for providers that \
                 price a longer break-even separately",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "prompt",
        kind: "path",
        default: "the embedded `src/prompt.md`",
        overrides: "—",
        effect: "the base prompt file; when set it replaces `<config>/prompt.md` (read only \
                 when `prompt` is unset); only `<cwd>/.kelir/prompt.md` wins over it",
        field_fn: "load_prompt",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "providers",
        kind: "table, keyed by provider name",
        default: "both providers, with their built-in base URL and key source",
        overrides: "—",
        effect: "per-provider settings (see the `providers.*` rows); any other table name is a \
                 config error",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "key",
        kind: "string",
        default: "—",
        overrides: "—",
        effect: "**refused**: a literal key is warned about and ignored (the environment, \
                 `key_cmd` and opencode's store are the three sources)",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "window",
        kind: "integer (tokens)",
        default: "unset: the provider's model metadata, else `128000`",
        overrides: "—",
        effect: "the context window for every model, for a provider that publishes none",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "model_windows",
        kind: "table of integers (tokens), keyed by model id",
        default: "empty",
        overrides: "—",
        effect: "per-model window overrides; they win over `window` and over the provider's \
                 metadata",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "auto_compact_at",
        kind: "float in `(0, 1]`",
        default: "`0.8`",
        overrides: "—",
        effect: "the fraction of the window at which compaction triggers automatically",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "compact_at_tokens",
        kind: "integer (tokens)",
        default: "`150000`; `0` disables the cap",
        overrides: "—",
        effect: "an absolute soft target: compaction triggers at \
                 `min(window * auto_compact_at, compact_at_tokens)`",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "compact_prompt",
        kind: "path",
        default: "the embedded default in `src/compact.rs`",
        overrides: "—",
        effect: "the summary prompt; `<config>/compact.md` is read when this is unset",
        field_fn: "load_compact_prompt",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "compact_keep",
        kind: "integer",
        default: "`4`",
        overrides: "—",
        effect: "how many messages stay verbatim after a compaction",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "skills_dirs",
        kind: "array of paths",
        default: "empty (`~/.claude/skills`, `<config>/skills`, `.claude/skills`, `.kelir/skills` \
                  are always searched)",
        overrides: "—",
        effect: "extra skills directories, named in the base prompt and searched read-only; `~/` \
                 and `$HOME/` expand",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "memory_dir",
        kind: "path",
        default: "unset: no memory",
        overrides: "—",
        effect: "a directory of `.md`/`.txt` notes loaded as `memory` sources: rendered once into \
                 the frozen system, and a note edited or created later arrives as a `source` \
                 message at the tail; the model writes memory with `edit`; `~/` expands, a \
                 relative path is the session cwd's",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "hint",
        kind: "table",
        default: "unset: the pushed hint is off",
        overrides: "—",
        effect: "`[hint]` with `model` (required to turn it on), `provider` (default: the session \
                 provider), `endpoint` (a base URL instead of the provider's; the provider's key is sent \
                 only when it has the same origin, else the call is keyless) and `timeout_ms` \
                 (default `2000`): one cheap scoring call per human prompt appends `might be \
                 relevant: name - description` as a `source` message of kind `hint`; it fails \
                 open",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "gates",
        kind: "table",
        default: "unset: no gates",
        overrides: "—",
        effect: "`[gates]`: executables run at `session_start`, `prompt_submit`, `before_tool`,                  `after_tool`, `turn_end`, `session_end`; each key takes a command string, a table                  (`command`, `name`, `tools`, `env`) or a list of them; a gate answers `allow`,                  `deny` or `rewrite` as JSON and fails open (see `gates`)",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "promote_after",
        kind: "integer (seconds)",
        default: "`600`; `0` disables promotion",
        overrides: "`KELIR_PROMOTE_AFTER`",
        effect: "a bash call still running after this long becomes a background job instead of \
                 being killed",
        field_fn: "promote_after",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "retry_base_ms",
        kind: "integer (ms)",
        default: "`2000`",
        overrides: "—",
        effect: "the first retry wait; each further attempt doubles up to `retry_cap_ms`",
        field_fn: "retry_policy",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "retry_cap_ms",
        kind: "integer (ms)",
        default: "`120000`",
        overrides: "—",
        effect: "the longest a backoff grows to; it must be at least `retry_base_ms`",
        field_fn: "retry_policy",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "retry_max_wait",
        kind: "integer (seconds)",
        default: "`1800`; `0` waits as long as the provider asks",
        overrides: "—",
        effect: "the total wait budget per request; running out pauses the turn \
                 (`reason: \"provider\"`)",
        field_fn: "retry_policy",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "retry_max_attempts",
        kind: "integer",
        default: "unset: only the budget and `retry_max_wait` bound the retries",
        overrides: "—",
        effect: "a count cap inside the budget (`3` = three tries and out)",
        field_fn: "retry_policy",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "warn_prompt_tokens",
        kind: "integer (tokens)",
        default: "`150000`; `0` turns the warning off",
        overrides: "—",
        effect: "when the last prompt is larger than this, one `warning` event and one dim line on                  the screen; it speaks again only after the prompt has fallen back under it.                  Never sent to the model",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "warn_cost",
        kind: "number (US dollars)",
        default: "unset: no cost warning",
        overrides: "—",
        effect: "when the session's cost passes this, one `warning` event and one dim line; never                  sent to the model. Only costs that are known count",
        field_fn: "warn_cost",
    },
    KeyDoc {
        scope: KeyScope::Top,
        name: "prices",
        kind: "table of tables `[prices.\"<model>\"]` with `input`, `output`, `cached_input`                (dollars per million tokens)",
        default: "empty: the built-in table (`src/spend.rs`, stamped with its date) applies",
        overrides: "—",
        effect: "overrides and extends the built-in price table. Used only when the provider                  reports no cost; a model with no price anywhere gets no estimate                  (`cost_source: \"unknown\"`). `cached_input` defaults to `input`",
        field_fn: "prices",
    },
    KeyDoc {
        scope: KeyScope::Provider,
        name: "base_url",
        kind: "string",
        default: "the provider's own endpoint",
        overrides: "`KELIR_BASE_URL` (for the session's provider)",
        effect: "where requests go: a proxy, a gateway, or a test's mock server",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Provider,
        name: "key_env",
        kind: "string",
        default: "`OPENROUTER_API_KEY` / `KELIR_GO_API_KEY`",
        overrides: "—",
        effect: "the variable the key is read from first",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Provider,
        name: "key_cmd",
        kind: "array of strings",
        default: "unset",
        overrides: "—",
        effect: "a command whose trimmed stdout is the key, when the variable says nothing; only \
                 its file name is recorded",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Provider,
        name: "key",
        kind: "string",
        default: "—",
        overrides: "—",
        effect: "**refused** here too: warned about and ignored",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Provider,
        name: "extra_headers",
        kind: "table of strings",
        default: "empty",
        overrides: "—",
        effect: "headers added to every request to this provider",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Provider,
        name: "reasoning_field",
        kind: "string",
        default: "unset: no field",
        overrides: "—",
        effect: "the request field a provider wants its effort in, when it is not the documented \
                 one",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Provider,
        name: "reasoning_effort",
        kind: "string: `low`, `medium`, `high`, `none`",
        default: "`medium`",
        overrides: "—",
        effect: "how much hidden reasoning to ask for; `none` sends no field, and an explicit \
                 value reaches a model even when the provider's list says nothing",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Provider,
        name: "session_header",
        kind: "string",
        default: "`x-opencode-session` for `opencode-go`",
        overrides: "—",
        effect: "the header carrying the session id, for a provider that requires one",
        field_fn: "load",
    },
    KeyDoc {
        scope: KeyScope::Provider,
        name: "max_concurrent_requests",
        kind: "integer",
        default: "unset: no limit",
        overrides: "—",
        effect: "at most this many requests in flight to this provider across every local kelir \
                 process (`0`/unset = off)",
        field_fn: "load",
    },
];

/// One event type: the fields a reader counts on, and what writes it.
#[derive(Debug, Clone, Copy)]
pub struct EventDoc {
    pub name: &'static str,
    pub fields: &'static str,
    pub written_by: &'static str,
}

/// Every `EventBody` variant, in `event_type()`'s order.
pub const EVENTS: &[EventDoc] = &[
    EventDoc {
        name: "session_started",
        fields: "`id`, `parent?`, `depth`, `cwd`, `os`, `kelir_version`, `provider`, `model`, \
                 `system{text,hash}`, `sources[]`, `tools[]`, `reasoning_effort?`, `key_origin?`",
        written_by: "the first event of a session: the frozen request prefix",
    },
    EventDoc {
        name: "user",
        fields: "`text`, `from`, `delivery`, `attachments[]`",
        written_by: "a human message: the TUI, `kelir ctl say`, a job's wake-up, a channel",
    },
    EventDoc {
        name: "source",
        fields: "`kind`, `name`, `text?`, `path?`",
        written_by: "context that arrived after the start: a `hint`, a skill, an instruction or \
                     memory edit, an `attachment` or `attachment-note`, a channel message",
    },
    EventDoc {
        name: "step_started",
        fields: "`step`, `provider`, `model`, `request_hash`",
        written_by: "just before one provider request",
    },
    EventDoc {
        name: "assistant",
        fields: "`step`, `text`, `reasoning?`, `tool_calls[]`, `finish_reason?`",
        written_by: "the model's answer to one step",
    },
    EventDoc {
        name: "usage",
        fields: "`step`, `prompt_tokens`, `completion_tokens`, `cached_tokens`, \
                 `cache_write_tokens`, `cost`, `cost_source?`, `allowance?`, `kind?`",
        written_by: "the final stream chunk; `cost_source` says whether `cost` is the provider's \
                    own (`provider`), tokens times a known price (`table`) or not known \
                    (`unknown`, cost 0.0); `kind: \"compact\"` marks the summarising request and `kind: \"hint\"` the \
                    hint scorer's own call",
    },
    EventDoc {
        name: "warning",
        fields: "`kind`, `message`, `value`, `threshold`",
        written_by: "the session when the last prompt passes `warn_prompt_tokens` or the cost \
                    passes `warn_cost`, once per crossing; never rendered into the conversation",
    },
    EventDoc {
        name: "verb",
        fields: "`name`, `args`, `from`, `boundary`, `error?`",
        written_by: "every verb received, applied or not (`boundary: \"rejected\"` for a refusal)",
    },
    EventDoc {
        name: "tool_started",
        fields: "`call_id`, `name`, `input`, `pid?`, `dir?`",
        written_by: "intent: written before the tool's process starts",
    },
    EventDoc {
        name: "tool_settled",
        fields: "`call_id`, `outcome`, `exit_code?`, `bytes`, `lines`, `out_path`, `spilled`, \
                 `result`",
        written_by: "the call's one commit; `result` is what the model sees",
    },
    EventDoc {
        name: "turn_ended",
        fields: "`outcome`, `reason?`, `final_text`",
        written_by: "the end of a turn; `reason` distinguishes `steps` from `provider` pausing",
    },
    EventDoc {
        name: "mcp_started",
        fields: "`mcp_id`, `server`, `tool`, `args`, `parent_call_id?`",
        written_by: "a `kelir mcp <server> <tool>` call reached the session broker with the session token (intent: written before the server is asked); `args` is the arguments' shape (keys kept, each string replaced by `<string, N bytes>`), never their values",
    },
    EventDoc {
        name: "mcp_settled",
        fields: "`mcp_id`, `server`, `tool`, `outcome`, `exit_code`, `bytes`, `out_path`, `parent_call_id?`",
        written_by: "the call's one commit; `outcome` is `ok`, `error`, `timeout`, `unavailable`, `disabled`, or `unknown` (a resume found no settlement), and the result is in the file `out_path` names",
    },
    EventDoc {
        name: "bg_started",
        fields: "`bg_id`, `call_id?`, `cmd`, `pid?`, `out_path`, `pubkey?`",
        written_by: "a promoted bash call or an explicit `kelir bg run`",
    },
    EventDoc {
        name: "bg_settled",
        fields: "`bg_id`, `call_id?`, `cmd`, `exit_code?`, `state`, `out_path`",
        written_by: "a job that settled while this session owned its log (`state`: `exited` or \
                     `cancelled`)",
    },
    EventDoc {
        name: "gate",
        fields: "`gate`, `event`, `decision`, `reason`, `ms`, `rewritten?`, `error?`",
        written_by: "every gate run, including the ones that failed open (`error` says why: crash, timeout, garbage)",
    },
    EventDoc {
        name: "compacted",
        fields: "`through_seq`, `summary`, `pinned_seqs[]`, `by`, `tokens_before`, \
                 `tokens_after`, `cache_restart`",
        written_by: "a compaction; the fold replaces everything up to `through_seq` except the \
                     pins",
    },
    EventDoc {
        name: "resumed",
        fields: "`from_seq`, `unsettled[]`",
        written_by: "a session reopened from its log",
    },
    EventDoc {
        name: "error",
        fields: "`where`, `message`, `retryable`, `status?`, `attempt?`, `retry_in_ms?`",
        written_by: "provider errors, broken sources, refused verbs; never a response body and \
                     never a key",
    },
    EventDoc {
        name: "session_ended",
        fields: "`reason`",
        written_by: "the end of the session",
    },
];
