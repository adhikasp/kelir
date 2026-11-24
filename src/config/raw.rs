//! Raw TOML shapes, defaults and provider constants.
use crate::config::types::*;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// `[compaction]`: when and how a long conversation is summarised.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RawCompaction {
    pub(super) auto_at: Option<f64>,
    pub(super) at_tokens: Option<u64>,
    pub(super) prompt: Option<String>,
    pub(super) keep: Option<usize>,
}

/// `[retry]`: the backoff schedule and budget.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RawRetry {
    pub(super) base_ms: Option<u64>,
    pub(super) cap_ms: Option<u64>,
    pub(super) max_wait: Option<u64>,
    pub(super) max_attempts: Option<u32>,
}

/// `[children]`: the caps the root enforces on its descendants.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RawChildren {
    pub(super) max_depth: Option<u32>,
    pub(super) max_per_agent: Option<u32>,
    pub(super) max_live_descendants: Option<u32>,
}

/// `[channel]`: the conversation and rate limits of a channel bridge.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RawChannel {
    pub(super) max_active: Option<usize>,
    pub(super) per_conversation_rpm: Option<u32>,
    pub(super) global_rpm: Option<u32>,
    pub(super) fallback: Option<String>,
    pub(super) spool_limit: Option<u64>,
}

/// `[notify]`: event hooks and the optional OSC terminal record.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct RawNotify {
    pub(super) terminal: bool,
    pub(super) terminal_kind: Option<String>,
    pub(super) rate_limit: Option<u64>,
    pub(super) title: Option<String>,
    #[serde(flatten)]
    pub(super) hooks: BTreeMap<String, toml::Value>,
}

/// `[mcp.servers.<name>]`: one place for MCP servers.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RawMcp {
    pub(super) servers: BTreeMap<String, RawMcpServer>,
}

/// One MCP server. The name is the CLI word and a directory name, so it is
/// validated at load time (see [`crate::features::mcp::config::valid_name`]).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RawMcpServer {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub description: String,
    pub channel: bool,
    pub timeout: Option<u64>,
    pub reply_tool: Option<String>,
    pub declare_capabilities: Vec<String>,
}

/// Every table is strict: an unknown key is the ordinary unknown-key error,
/// with no "was removed" message for any name.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RawConfig {
    pub(super) provider: Option<String>,
    pub(super) model: Option<String>,
    pub(super) shell: Option<ShellSpec>,
    pub(super) spill_limit: Option<usize>,
    pub(super) max_steps: Option<u32>,
    pub(super) cache_ttl: Option<String>,
    pub(super) prompt: Option<String>,
    pub(super) window: Option<u64>,
    pub(super) model_windows: BTreeMap<String, u64>,
    pub(super) promote_after: Option<u64>,
    pub(super) warn_prompt_tokens: Option<u64>,
    pub(super) skills_dirs: Vec<PathBuf>,
    pub(super) memory_dir: Option<PathBuf>,
    pub(super) trusted_projects: Vec<PathBuf>,
    pub(super) gates: Option<toml::Value>,
    pub(super) providers: BTreeMap<String, RawProvider>,
    pub(super) compaction: RawCompaction,
    pub(super) retry: RawRetry,
    pub(super) children: RawChildren,
    pub(super) channel: RawChannel,
    pub(super) notify: RawNotify,
    pub(super) mcp: RawMcp,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(super) enum ShellSpec {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RawProvider {
    pub(super) base_url: Option<String>,
    pub(super) key_env: Option<String>,
    pub(super) key_cmd: Option<Vec<String>>,
    pub(super) extra_headers: BTreeMap<String, String>,
    pub(super) reasoning_field: Option<String>,
    pub(super) reasoning_effort: Option<String>,
    pub(super) session_header: Option<String>,
    pub(super) max_concurrent_requests: Option<u32>,
}

pub const PROVIDER_OPENROUTER: &str = "openrouter";
pub const PROVIDER_GO: &str = "opencode-go";

pub const PROVIDERS: [&str; 2] = [PROVIDER_OPENROUTER, PROVIDER_GO];

/// The error `provider = "..."` and `--provider` give for anything else: it names both providers,
/// because a maintainer's old config said `kelir` and the only useful answer is what the two real ones
pub fn unknown_provider(name: &str) -> ConfigError {
    ConfigError(format!(
        "unknown provider `{name}`: kelir has exactly two, `{PROVIDER_OPENROUTER}` and `{PROVIDER_GO}`"
    ))
}

/// An unknown `[providers.<name>]` table is the same error as an unknown
/// `provider = "…"`: exactly two names exist, and neither is special.
pub fn unknown_provider_table(name: &str) -> ConfigError {
    unknown_provider(name)
}

pub(super) fn builtin_provider(name: &str) -> Option<RawProvider> {
    match name {
        PROVIDER_OPENROUTER => Some(RawProvider {
            base_url: Some("https://openrouter.ai/api/v1".into()),
            key_env: Some(crate::provider::keys::OPENROUTER_ENV.into()),
            ..RawProvider::default()
        }),
        PROVIDER_GO => Some(RawProvider {
            base_url: Some("https://opencode.ai/inference/go/v1".into()),
            // `KELIR_GO_API_KEY` is the *override*; the default source is opencode's own store.
            key_env: Some(crate::provider::keys::GO_ENV.into()),
            session_header: Some("x-opencode-session".into()),
            ..RawProvider::default()
        }),
        _ => None,
    }
}
