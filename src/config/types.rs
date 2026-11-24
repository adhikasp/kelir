//! Public config types and the accepted-key list.
use crate::config::raw::*;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The base prompt, embedded at build time (`src/prompt.md`, at most 1 KB).
pub const DEFAULT_PROMPT: &str = include_str!("../prompt.md");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    OpenRouter,
    OpencodeGo,
}

impl ProviderKind {
    /// The one name this provider is configured and addressed by.
    pub fn name(self) -> &'static str {
        match self {
            ProviderKind::OpenRouter => PROVIDER_OPENROUTER,
            ProviderKind::OpencodeGo => PROVIDER_GO,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    pub base_url: String,
    /// The variable this provider's key is read from, when one is named
    /// ([`crate::provider::keys`] has the precedence per provider).
    pub key_env: Option<String>,
    /// A command whose trimmed stdout is the key, when the environment says
    /// nothing. Never a literal in the config file (see [`load`]).
    pub key_cmd: Option<Vec<String>>,
    /// The resolved key. Filled at session start by [`crate::provider::keys`] and
    /// held in memory only; never serialised, logged or put on a command line.
    pub key: Option<String>,
    /// Where the key came from (`env OPENROUTER_API_KEY`, `opencode auth export
    /// opencode`), for `kelir doctor` and a one-line notice. Never the secret.
    pub key_origin: Option<String>,
    /// When the source publishes an expiry (opencode's store does), epoch ms.
    pub key_expires_at_ms: Option<u64>,
    pub extra_headers: BTreeMap<String, String>,
    pub reasoning_field: Option<String>,
    /// How much hidden reasoning the model should spend on this session's requests (`low`, `medium`,
    /// `high`), sent as `reasoning_effort`.
    pub reasoning_effort: Option<String>,
    /// Whether [`ProviderConfig::reasoning_effort`] came from the config file rather than from kelir's own
    /// default (`medium`).
    pub reasoning_effort_explicit: bool,
    pub session_header: Option<String>,
    /// Client-side concurrency governor: at most this many requests in flight
    /// per provider across every local kelir process (`0`/unset = off).
    pub max_concurrent_requests: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub provider: String,
    pub model: String,
    /// argv prefix; the command is appended (so `bash -c` or `pwsh -Command`).
    pub shell: Vec<String>,
    pub spill_limit: usize,
    /// The cap on one turn's steps: `None` means unlimited, which is the default.
    pub max_steps: Option<u32>,
    pub cache_ttl: Option<String>,
    pub system_prompt: String,
    /// The file `system_prompt` was read from (`prompt` or `prompt.md`), when it came from one: `sources[]`
    /// records it, so a log reader can tell a file's base prompt from the embedded default .
    pub prompt_path: Option<PathBuf>,
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Context window override for every model (`window`), when set.
    pub window: Option<u64>,
    /// Per-model window overrides (`[model_windows]`).
    pub model_windows: BTreeMap<String, u64>,
    pub auto_compact_at: f64,
    /// An absolute soft target in tokens: compaction triggers at `min(window * auto_compact_at,
    /// compact_at_tokens)`, so a huge window does not carry a 400K-token context.
    pub compact_at_tokens: u64,
    /// The summary prompt (`[compaction] prompt`/`compact.md`, else embedded).
    pub compact_prompt: String,
    /// Extra skills directories (`skills_dirs`), machine layer: they are named
    /// in the base prompt and searched read-only.
    pub skills_dirs: Vec<PathBuf>,
    /// Messages kept verbatim after a compaction.
    pub compact_keep: usize,
    /// The memory directory (`memory_dir`): its files are loaded as sources.
    /// `None` is memory off.
    pub memory_dir: Option<PathBuf>,
    /// A bash call still running after this long is promoted to a background job instead of being killed
    /// (`config.toml` `promote_after`, seconds; `KELIR_PROMOTE_AFTER` overrides it; zero disables
    pub promote_after: std::time::Duration,
    /// What to retry and how long to wait (`[retry]` table).
    pub retry: crate::provider::retry::Policy,
    /// Warn (log and screen, never the model) when the last prompt is larger than this many
    /// tokens (`warn_prompt_tokens`, default 150000; `None` = off).
    pub warn_prompt_tokens: Option<u64>,
    /// The child caps (`[children] max_depth`, `max_per_agent`, `max_live_descendants`):
    /// enforced by the root session's registry.
    pub children: crate::features::children::Caps,
    /// Gates declared under `[gates]`; skills add theirs at session start.
    pub gates: Vec<crate::features::gates::GateDef>,
    /// Gates declared by the *project's* `.kelir/config.toml`. They run only when the project is
    /// trusted: `trusted_projects` (machine layer) names it, or `KELIR_TRUST_PROJECT` is set.
    pub project_gates: Vec<crate::features::gates::GateDef>,
    /// `trusted_projects`: directories whose project-layer gates may run.
    pub trusted_projects: Vec<PathBuf>,
    /// Channel mode: the conversation concurrency, rate limits, fallback
    /// text and spool bound. Every field has a safe default.
    pub channel: crate::features::channel::Config,
    /// Notification hooks and terminal records. Nothing is enabled by
    /// default.
    pub notify: crate::features::notify::Config,
    /// `[mcp.servers.*]`: the MCP servers this machine/project configures.
    pub mcp: BTreeMap<String, crate::features::mcp::config::ServerConfig>,
}

/// Every top-level and per-table key the config reader accepts, for the docs
/// drift test. The test below checks it against the serde structs.
pub const KEYS: &[&str] = &[
    "provider",
    "model",
    "shell",
    "spill_limit",
    "max_steps",
    "cache_ttl",
    "prompt",
    "window",
    "model_windows",
    "promote_after",
    "warn_prompt_tokens",
    "skills_dirs",
    "memory_dir",
    "gates",
    "trusted_projects",
    "providers",
    "compaction",
    "retry",
    "children",
    "channel",
    "notify",
    "mcp",
    // compaction
    "auto_at",
    "at_tokens",
    "prompt",
    "keep",
    // retry
    "base_ms",
    "cap_ms",
    "max_wait",
    "max_attempts",
    // children
    "max_depth",
    "max_per_agent",
    "max_live_descendants",
    // channel
    "max_active",
    "per_conversation_rpm",
    "global_rpm",
    "fallback",
    "spool_limit",
    // notify
    "hooks",
    "terminal",
    "terminal_kind",
    "rate_limit",
    "title",
    // mcp
    "servers",
    "command",
    "args",
    "env",
    "description",
    "reply_tool",
    "declare_capabilities",
    "channel",
    "timeout",
    // provider table
    "base_url",
    "key_env",
    "key_cmd",
    "extra_headers",
    "reasoning_field",
    "reasoning_effort",
    "session_header",
    "max_concurrent_requests",
];

#[derive(Debug)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ConfigError {}
