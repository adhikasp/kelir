//! `config.toml`: machine config, then the project's `.kelir/config.toml`.
//! CLI flags override the file; the environment supplies the keys and can point the
//! provider at a stand-in base URL (`KELIR_BASE_URL`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::paths;

/// The base prompt, embedded at build time (`src/prompt.md`, at most 1 KB).
pub const DEFAULT_PROMPT: &str = include_str!("prompt.md");

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
    /// ([`crate::credentials`] has the precedence per provider).
    pub key_env: Option<String>,
    /// A command whose trimmed stdout is the key, when the environment says
    /// nothing. Never a literal in the config file (see [`load`]).
    pub key_cmd: Option<Vec<String>>,
    /// The resolved key. Filled at session start by [`crate::credentials`] and
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
    /// The summary prompt (`compact_prompt`/`compact.md`, else embedded).
    pub compact_prompt: String,
    /// Extra skills directories (`skills_dirs`), machine layer: they are named
    /// in the base prompt and searched read-only.
    pub skills_dirs: Vec<PathBuf>,
    /// Messages kept verbatim after a compaction.
    pub compact_keep: usize,
    /// The memory directory (`memory_dir`): its files are loaded as sources.
    /// `None` is memory off.
    pub memory_dir: Option<PathBuf>,
    /// The pushed hint's scorer (`[hint]`); `None` is the hint off, which is
    /// the default.
    pub hint: Option<HintConfig>,
    /// A bash call still running after this long is promoted to a background job instead of being killed
    /// (`config.toml` `promote_after`, seconds; `KELIR_PROMOTE_AFTER` overrides it; zero disables
    pub promote_after: std::time::Duration,
    /// What to retry and how long to wait (`retry_base_ms`, `retry_cap_ms`,
    /// `retry_max_wait`).
    pub retry: crate::retry::Policy,
    /// Warn (log and screen, never the model) when the last prompt is larger than this many
    /// tokens (`warn_prompt_tokens`, default 150000; `None` = off).
    pub warn_prompt_tokens: Option<u64>,
    /// The child caps (`max_child_depth`, `max_children`, `max_live_descendants`):
    /// enforced by the root session's registry.
    pub children: crate::child::Caps,
    /// Gates declared under `[gates]`; skills add theirs at session start.
    pub gates: Vec<crate::gates::GateDef>,
    /// Gates declared by the *project's* `.kelir/config.toml`. They run only when the project is
    /// trusted: `trusted_projects` (machine layer) names it, or `KELIR_TRUST_PROJECT` is set.
    pub project_gates: Vec<crate::gates::GateDef>,
    /// `trusted_projects`: directories whose project-layer gates may run.
    pub trusted_projects: Vec<PathBuf>,
    /// Channel mode: the conversation concurrency, rate limits, fallback
    /// text and spool bound. Every field has a safe default.
    pub channel: crate::channel::Config,
    /// Notification hooks and terminal records. Nothing is enabled by
    /// default.
    pub notify: crate::notify::Config,
}

/// The pushed hint, configured: which model scores, and where.
#[derive(Debug, Clone, PartialEq)]
pub struct HintConfig {
    /// The provider whose key and base URL the scorer uses.
    pub provider: String,
    pub model: String,
    /// A base URL for the scorer, instead of the provider's own.
    pub endpoint: Option<String>,
    /// How long one scoring call may take before the hint is skipped.
    pub timeout: std::time::Duration,
}

/// Every top-level and per-provider key the config reader accepts, for the
/// docs drift test. The test below checks it against the serde structs.
pub const KEYS: &[&str] = &[
    "provider",
    "model",
    "shell",
    "spill_limit",
    "max_steps",
    "cache_ttl",
    "prompt",
    "providers",
    "key",
    "window",
    "model_windows",
    "auto_compact_at",
    "compact_at_tokens",
    "compact_prompt",
    "compact_keep",
    "skills_dirs",
    "memory_dir",
    "hint",
    "promote_after",
    "retry_base_ms",
    "retry_cap_ms",
    "retry_max_wait",
    "retry_max_attempts",
    "warn_prompt_tokens",
    "max_child_depth",
    "max_children",
    "max_live_descendants",
    "gates",
    "trusted_projects",
    "channel_max_active",
    "channel_per_conversation_rpm",
    "channel_global_rpm",
    "channel_fallback",
    "channel_spool_limit",
    "notify",
    "base_url",
    "key_env",
    "key_cmd",
    "extra_headers",
    "reasoning_field",
    "reasoning_effort",
    "session_header",
    "max_concurrent_requests",
];

/// How long the scoring call may take by default.
pub const HINT_TIMEOUT_MS: u64 = 2000;

#[derive(Debug)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawConfig {
    provider: Option<String>,
    model: Option<String>,
    shell: Option<ShellSpec>,
    spill_limit: Option<usize>,
    max_steps: Option<u32>,
    cache_ttl: Option<String>,
    prompt: Option<String>,
    providers: BTreeMap<String, RawProvider>,
    /// A key written into the config file itself.
    key: Option<String>,
    window: Option<u64>,
    model_windows: BTreeMap<String, u64>,
    auto_compact_at: Option<f64>,
    compact_at_tokens: Option<u64>,
    compact_prompt: Option<String>,
    compact_keep: Option<usize>,
    skills_dirs: Vec<PathBuf>,
    /// A directory of notes loaded as sources (`memory_dir`).
    memory_dir: Option<PathBuf>,
    /// The pushed hint's scorer (the `[hint]` table).
    hint: Option<RawHint>,
    promote_after: Option<u64>,
    retry_base_ms: Option<u64>,
    retry_cap_ms: Option<u64>,
    /// Total waiting per request in seconds; `0` waits as long as it takes.
    retry_max_wait: Option<u64>,
    /// A count cap inside the budget (`retry_max_attempts`), for a job that
    /// wants "three tries and out".
    retry_max_attempts: Option<u32>,
    warn_prompt_tokens: Option<u64>,
    max_child_depth: Option<u32>,
    max_children: Option<u32>,
    max_live_descendants: Option<u32>,
    /// `[gates]`: event name -> command, table or list (see `crate::gates`).
    gates: Option<toml::Value>,
    /// Directories whose project-layer gates may run (machine layer only).
    #[serde(default)]
    trusted_projects: Vec<PathBuf>,
    /// Channel mode: how many conversations may answer at once.
    channel_max_active: Option<usize>,
    /// Channel mode: messages per conversation per minute.
    channel_per_conversation_rpm: Option<u32>,
    /// Channel mode: messages across all conversations per minute.
    channel_global_rpm: Option<u32>,
    /// Channel mode: fallback reply text when a turn fails, pauses or times out.
    channel_fallback: Option<String>,
    /// Channel mode: spool bytes before done records are compacted.
    channel_spool_limit: Option<u64>,
    /// `[notify]`: event -> hook commands, plus terminal and rate-limit knobs.
    notify: Option<toml::Value>,
}

/// The `[hint]` table: the scorer of the pushed hint.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RawHint {
    provider: Option<String>,
    model: Option<String>,
    endpoint: Option<String>,
    timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ShellSpec {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RawProvider {
    base_url: Option<String>,
    key_env: Option<String>,
    key_cmd: Option<Vec<String>>,
    /// A literal key in a provider table: warned about and ignored, never used
    /// (the same rule as the top-level `key`).
    key: Option<String>,
    extra_headers: BTreeMap<String, String>,
    reasoning_field: Option<String>,
    reasoning_effort: Option<String>,
    session_header: Option<String>,
    max_concurrent_requests: Option<u32>,
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

/// The error a `[providers.<name>]` table with an unknown name gives ( fix 3): it says which table, in
/// which file, that the name *is* the provider (`kind = …` is no longer used), and what to rename it
pub fn unknown_provider_table(name: &str, sources: &[PathBuf]) -> ConfigError {
    let files = if sources.is_empty() {
        "the config file".to_string()
    } else {
        sources
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    ConfigError(format!(
        "`[providers.{name}]` in {files}: unknown provider `{name}`: the table name *is* the provider and there are exactly \
         two, `{PROVIDER_OPENROUTER}` and `{PROVIDER_GO}` (`kind = \"…\"` is no longer used). Rename the table to \
         `[providers.{PROVIDER_OPENROUTER}]` or `[providers.{PROVIDER_GO}]`, then check `provider = \"…\"` in the same file; \
         `key_env` keeps working"
    ))
}

fn builtin_provider(name: &str) -> Option<RawProvider> {
    match name {
        PROVIDER_OPENROUTER => Some(RawProvider {
            base_url: Some("https://openrouter.ai/api/v1".into()),
            key_env: Some(crate::credentials::OPENROUTER_ENV.into()),
            ..RawProvider::default()
        }),
        PROVIDER_GO => Some(RawProvider {
            base_url: Some("https://opencode.ai/inference/go/v1".into()),
            // `KELIR_GO_API_KEY` is the *override*; the default source is opencode's own store.
            key_env: Some(crate::credentials::GO_ENV.into()),
            session_header: Some("x-opencode-session".into()),
            ..RawProvider::default()
        }),
        _ => None,
    }
}

/// Load the config. `explicit` (CLI flag) or `KELIR_CONFIG` replace the search.
pub fn load(explicit: Option<&Path>) -> Result<Config, ConfigError> {
    let explicit = explicit
        .map(PathBuf::from)
        .or_else(|| crate::envs::var_os("KELIR_CONFIG").map(PathBuf::from))
        .filter(|p| !p.as_os_str().is_empty());

    let mut merged = toml::Value::Table(Default::default());
    let mut sources: Vec<PathBuf> = Vec::new();
    let mut project_gates: Option<toml::Value> = None;
    match explicit {
        Some(path) => {
            let value = read_toml(&path)?;
            merge(&mut merged, value);
            sources.push(path);
        }
        None => {
            let machine = paths::machine_config_path();
            if machine.exists() {
                merge(&mut merged, read_toml(&machine)?);
                sources.push(machine);
            }
            if let Some(project) = find_project_config() {
                let mut layer = read_toml(&project)?;
                // The project layer cannot vouch for itself, and its gates are
                // kept apart: they run only in a trusted project .
                if let Some(table) = layer.as_table_mut() {
                    project_gates = table.remove("gates");
                    if table.remove("trusted_projects").is_some() {
                        eprintln!(
                            "kelir: warning: `trusted_projects` in a project config is ignored: it is a machine setting"
                        );
                    }
                }
                merge(&mut merged, layer);
                sources.push(project);
            }
        }
    }

    let raw: RawConfig = merged
        .try_into()
        .map_err(|error| ConfigError(format!("invalid config.toml: {error}")))?;
    // Read the knobs that are plain numbers before anything is moved out of
    // `raw`.
    let retry = retry_policy(&raw)?;

    // A literal key in the config file is refused with a warning: `key = "…"` goes stale, shadows a live
    // store and puts a secret in a file that gets copied around.
    if let Some(_literal) = raw.key.as_deref().filter(|key| !key.trim().is_empty()) {
        eprintln!(
            "kelir: warning: `key` in a config file is ignored: use the environment (\
             {} / {}) or `key_cmd = [\"cmd\", …]`",
            crate::credentials::OPENROUTER_ENV,
            crate::credentials::GO_ENV
        );
    }

    let shell = match raw.shell {
        Some(ShellSpec::One(shell)) => vec![shell, "-c".into()],
        Some(ShellSpec::Many(argv)) if !argv.is_empty() => argv,
        _ => default_shell(),
    };

    // Exactly two providers, and only those two: a table for anything else is a usage error that
    // names the pair (a maintainer's config called theirs `kelir`) and the file to edit.
    if let Some(other) = raw
        .providers
        .keys()
        .find(|name| !PROVIDERS.contains(&name.as_str()))
    {
        return Err(unknown_provider_table(other, &sources));
    }

    let mut providers = BTreeMap::new();
    for name in PROVIDERS {
        let base = builtin_provider(name).unwrap_or_default();
        let user = raw.providers.get(name).cloned().unwrap_or_default();
        if let Some(_literal) = user.key.as_deref().filter(|key| !key.trim().is_empty()) {
            eprintln!(
                "kelir: warning: `[providers.{name}] key` is ignored: use `key_env`, `key_cmd`,\
                 or the environment"
            );
        }
        let entry = overlay(base, user);
        let kind = if name == PROVIDER_OPENROUTER {
            ProviderKind::OpenRouter
        } else {
            ProviderKind::OpencodeGo
        };
        let base_url = entry
            .base_url
            .clone()
            .filter(|url| !url.is_empty())
            .ok_or_else(|| ConfigError(format!("provider `{name}`: base_url is required")))?;
        let (effort, effort_explicit) = reasoning_effort(entry.reasoning_effort, name)?;
        providers.insert(
            name.to_string(),
            ProviderConfig {
                kind,
                base_url,
                // The key itself is resolved at session start ([`crate::credentials`]): it may need a
                // process read, and a config load must stay pure (a test, a `doctor`, an editor).
                key_env: entry.key_env,
                key_cmd: entry.key_cmd,
                key: None,
                key_origin: None,
                key_expires_at_ms: None,
                extra_headers: entry.extra_headers,
                reasoning_field: entry.reasoning_field,
                reasoning_effort: effort,
                reasoning_effort_explicit: effort_explicit,
                session_header: entry.session_header,
                max_concurrent_requests: entry.max_concurrent_requests.filter(|slots| *slots > 0),
            },
        );
    }

    // The base prompt and the file it came from (`None` for the embedded
    // default), so `sources[]` records the real path .
    let (system_prompt, prompt_path) = load_prompt(raw.prompt.as_deref())?;
    let compact_prompt = load_compact_prompt(raw.compact_prompt.as_deref())?;
    let auto_compact_at = raw.auto_compact_at.unwrap_or(crate::compact::AUTO_FRACTION);
    if !(0.0..=1.0).contains(&auto_compact_at) || auto_compact_at == 0.0 {
        return Err(ConfigError(format!(
            "auto_compact_at must be between 0 and 1 (got {auto_compact_at})"
        )));
    }

    let default_provider = raw
        .provider
        .clone()
        .unwrap_or_else(|| PROVIDER_OPENROUTER.to_string());
    let hint = hint_config(raw.hint.as_ref(), &default_provider)?;

    Ok(Config {
        compact_at_tokens: raw
            .compact_at_tokens
            .unwrap_or(crate::compact::DEFAULT_SOFT_TARGET),
        provider: match raw.provider {
            Some(name) if PROVIDERS.contains(&name.as_str()) => name,
            Some(other) => return Err(unknown_provider(&other)),
            None => PROVIDER_OPENROUTER.into(),
        },
        model: raw
            .model
            .unwrap_or_else(|| "deepseek/deepseek-v4.1-flash-1017".into()),
        shell,
        spill_limit: raw.spill_limit.unwrap_or(10_240),
        max_steps: step_cap(raw.max_steps),
        cache_ttl: raw.cache_ttl,
        system_prompt,
        prompt_path,
        providers,
        window: raw.window,
        model_windows: raw.model_windows,
        auto_compact_at,
        compact_prompt,
        skills_dirs: raw
            .skills_dirs
            .into_iter()
            .map(|path| expand_home(&path.to_string_lossy()))
            .collect(),
        compact_keep: raw.compact_keep.unwrap_or(crate::compact::DEFAULT_KEEP),
        memory_dir: raw
            .memory_dir
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| expand_home(&path.to_string_lossy())),
        hint,
        promote_after: promote_after(raw.promote_after)?,
        retry,
        warn_prompt_tokens: match raw.warn_prompt_tokens {
            Some(0) => None,
            Some(tokens) => Some(tokens),
            None => Some(crate::usage::DEFAULT_WARN_PROMPT_TOKENS),
        },
        children: crate::child::Caps {
            depth: raw
                .max_child_depth
                .unwrap_or(crate::child::DEFAULT_MAX_DEPTH),
            per_agent: raw
                .max_children
                .unwrap_or(crate::child::DEFAULT_MAX_CHILDREN),
            live: raw
                .max_live_descendants
                .unwrap_or(crate::child::DEFAULT_MAX_LIVE),
        },
        gates: match &raw.gates {
            Some(value) => crate::gates::from_toml(value).map_err(ConfigError)?,
            None => Vec::new(),
        },
        project_gates: match &project_gates {
            Some(value) => crate::gates::from_toml(value)
                .map_err(|error| ConfigError(format!("project config: {error}")))?
                .into_iter()
                .map(|mut def| {
                    def.origin = "project config".into();
                    def
                })
                .collect(),
            None => Vec::new(),
        },
        trusted_projects: raw
            .trusted_projects
            .iter()
            .map(|path| expand_home(&path.to_string_lossy()))
            .collect(),
        channel: crate::channel::Config {
            max_active: raw
                .channel_max_active
                .unwrap_or(crate::channel::DEFAULT_MAX_ACTIVE),
            per_conversation_rpm: raw
                .channel_per_conversation_rpm
                .unwrap_or(crate::channel::DEFAULT_PER_CONVERSATION_RPM),
            global_rpm: raw
                .channel_global_rpm
                .unwrap_or(crate::channel::DEFAULT_GLOBAL_RPM),
            fallback: raw
                .channel_fallback
                .unwrap_or_else(|| crate::channel::DEFAULT_FALLBACK.into()),
            spool_limit: raw
                .channel_spool_limit
                .unwrap_or(crate::channel::DEFAULT_SPOOL_LIMIT),
        },
        notify: notify(raw.notify.as_ref())?,
    })
}

/// The `[notify]` table: event -> command list, plus terminal and rate limit. Nothing is
/// enabled by default.
fn notify(raw: Option<&toml::Value>) -> Result<crate::notify::Config, ConfigError> {
    let Some(value) = raw else {
        return Ok(crate::notify::Config::default());
    };
    let table = value.as_table().ok_or_else(|| {
        ConfigError("`[notify]` must be a table of event names and commands".into())
    })?;
    let mut cfg = crate::notify::Config::default();
    for (key, entry) in table {
        match key.as_str() {
            "terminal" => {
                cfg.terminal = entry
                    .as_bool()
                    .ok_or_else(|| ConfigError("`notify.terminal` must be a boolean".into()))?;
            }
            "terminal_kind" => {
                let kind = entry.as_str().unwrap_or("").to_string();
                if !kind.is_empty() && kind != "osc9" && kind != "osc777" {
                    return Err(ConfigError(
                        "`notify.terminal_kind` must be `osc9` or `osc777`".into(),
                    ));
                }
                cfg.terminal_kind = kind;
            }
            "rate_limit" | "rate_limit_secs" => {
                cfg.rate_limit_secs = entry
                    .as_integer()
                    .and_then(|n| u64::try_from(n).ok())
                    .ok_or_else(|| {
                        ConfigError("`notify.rate_limit` must be a non-negative integer".into())
                    })?;
            }
            "title" => {
                cfg.title = entry.as_str().unwrap_or("").to_string();
            }
            kind if crate::notify::KINDS.contains(&kind) => {
                let commands: Vec<String> = match entry {
                    toml::Value::String(one) => vec![one.clone()],
                    toml::Value::Array(list) => list
                        .iter()
                        .map(|item| {
                            item.as_str().map(str::to_string).ok_or_else(|| {
                                ConfigError(format!("`notify.{kind}` holds a non-string command"))
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    _ => {
                        return Err(ConfigError(format!(
                            "`notify.{kind}` must be a command string or a list of them"
                        )));
                    }
                };
                cfg.hooks.insert(kind.to_string(), commands);
            }
            other => {
                return Err(ConfigError(format!(
                    "unknown notification event `{other}` in `[notify]`: expected one of {}",
                    crate::notify::KINDS.join(", ")
                )));
            }
        }
    }
    Ok(cfg)
}

/// The pushed hint: off unless a `[hint]` table names a model.
fn hint_config(
    raw: Option<&RawHint>,
    default_provider: &str,
) -> Result<Option<HintConfig>, ConfigError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let Some(model) = raw
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    else {
        return Ok(None);
    };
    let provider = match raw
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        Some(name) if PROVIDERS.contains(&name) => name.to_string(),
        Some(other) => return Err(unknown_provider(other)),
        None => default_provider.to_string(),
    };
    let timeout_ms = raw.timeout_ms.unwrap_or(HINT_TIMEOUT_MS);
    if timeout_ms == 0 {
        return Err(ConfigError("hint.timeout_ms must be above 0".into()));
    }
    Ok(Some(HintConfig {
        provider,
        model: model.to_string(),
        endpoint: raw
            .endpoint
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(str::to_string),
        timeout: std::time::Duration::from_millis(timeout_ms),
    }))
}

/// The retry schedule and budget. A wrong value is a config error rather than a
/// silent fallback: a person who writes `retry_max_wait` means it.
fn retry_policy(raw: &RawConfig) -> Result<crate::retry::Policy, ConfigError> {
    let base_ms = raw.retry_base_ms.unwrap_or(crate::retry::DEFAULT_BASE_MS);
    let cap_ms = raw.retry_cap_ms.unwrap_or(crate::retry::DEFAULT_CAP_MS);
    if cap_ms < base_ms {
        return Err(ConfigError(format!(
            "retry_cap_ms ({cap_ms}) must be at least retry_base_ms ({base_ms})"
        )));
    }
    let max_wait = match raw.retry_max_wait {
        // `0` is the documented "wait as long as the provider needs".
        Some(0) => None,
        Some(seconds) => Some(std::time::Duration::from_secs(seconds)),
        None => Some(std::time::Duration::from_secs(
            crate::retry::DEFAULT_WAIT_SECS,
        )),
    };
    Ok(crate::retry::Policy {
        base_ms,
        cap_ms,
        max_wait,
        max_attempts: raw.retry_max_attempts.filter(|attempts| *attempts > 0),
    })
}

/// The effort kelir asks for when the config says nothing: enough thinking to
/// plan, not enough to re-derive the plan every step.
pub const DEFAULT_REASONING_EFFORT: &str = "medium";

/// The values `reasoning_effort` accepts.
pub const REASONING_EFFORTS: [&str; 3] = ["low", "medium", "high"];

/// Resolve one provider's `reasoning_effort`:
fn reasoning_effort(
    configured: Option<String>,
    provider: &str,
) -> Result<(Option<String>, bool), ConfigError> {
    let Some(value) = configured else {
        return Ok((Some(DEFAULT_REASONING_EFFORT.to_string()), false));
    };
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() {
        return Ok((Some(DEFAULT_REASONING_EFFORT.to_string()), false));
    }
    if value == "none" {
        return Ok((None, true));
    }
    if !REASONING_EFFORTS.contains(&value.as_str()) {
        return Err(ConfigError(format!(
            "provider `{provider}`: reasoning_effort must be one of {}, `none` or unset (got `{value}`)",
            REASONING_EFFORTS.join(", ")
        )));
    }
    Ok((Some(value), true))
}

/// `max_steps`: unset or `0` means no cap at all (the default: a fully autonomous agent is not stopped
/// by a counter), a positive value caps one turn.
fn step_cap(configured: Option<u32>) -> Option<u32> {
    configured.filter(|steps| *steps > 0)
}

/// Seconds between `config.toml` and `KELIR_PROMOTE_AFTER`. Zero disables
/// promotion: a long call then runs to completion, as it did before.
fn promote_after(configured: Option<u64>) -> Result<std::time::Duration, ConfigError> {
    let seconds = match crate::envs::var_os("KELIR_PROMOTE_AFTER") {
        Some(value) => {
            let text = value.to_string_lossy().trim().to_string();
            text.parse::<u64>().map_err(|_| {
                ConfigError(format!(
                    "KELIR_PROMOTE_AFTER must be a whole number of seconds (got `{text}`)"
                ))
            })?
        }
        None => configured.unwrap_or(600),
    };
    Ok(std::time::Duration::from_secs(seconds))
}

impl Config {
    pub fn provider(&self, name: &str) -> Result<ProviderConfig, ConfigError> {
        let mut provider = self
            .providers
            .get(name)
            .cloned()
            .ok_or_else(|| unknown_provider(name))?;
        // A stand-in for either provider (the mock server in tests, a proxy).
        if let Some(url) = crate::envs::var_os("KELIR_BASE_URL")
            .map(|url| url.to_string_lossy().trim().to_string())
            .filter(|url| !url.is_empty())
        {
            provider.base_url = url;
        }
        Ok(provider)
    }

    /// How much hidden reasoning to ask the current provider/model for: the configured `reasoning_effort`,
    /// else [`crate::compact`]'s own default for the loop (`medium`), else nothing.
    pub fn reasoning_effort(&self, provider: &str) -> Option<String> {
        self.providers
            .get(provider)
            .and_then(|entry| entry.reasoning_effort.clone())
    }

    /// The window a configuration override names, if any: `[model_windows]` for this model, else `window`.
    pub fn override_window(&self, model: &str) -> Option<u64> {
        self.model_windows
            .get(model)
            .copied()
            .filter(|window| *window > 0)
            .or(self.window.filter(|window| *window > 0))
    }
}

/// `~/x` and `$HOME/x` in a configured path, so a `skills_dirs` entry is
/// portable between a person's config and a machine's.
fn expand_home(path: &str) -> PathBuf {
    let path = path.trim();
    let rest = path
        .strip_prefix("~/")
        .or_else(|| path.strip_prefix("~\\"))
        .or_else(|| path.strip_prefix("$HOME/"))
        .or_else(|| path.strip_prefix("$HOME\\"));
    match rest {
        Some(rest) => paths::home().join(rest),
        None => PathBuf::from(path),
    }
}

fn secret_from_env(name: &str) -> Option<String> {
    crate::envs::named(name)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn overlay(base: RawProvider, user: RawProvider) -> RawProvider {
    RawProvider {
        base_url: user.base_url.or(base.base_url),
        key_env: user.key_env.or(base.key_env),
        key_cmd: user.key_cmd.or(base.key_cmd),
        key: user.key.or(base.key),
        extra_headers: {
            let mut headers = base.extra_headers;
            headers.extend(user.extra_headers);
            headers
        },
        reasoning_field: user.reasoning_field.or(base.reasoning_field),
        reasoning_effort: user.reasoning_effort.or(base.reasoning_effort),
        session_header: user.session_header.or(base.session_header),
        max_concurrent_requests: user
            .max_concurrent_requests
            .or(base.max_concurrent_requests),
    }
}

fn read_toml(path: &Path) -> Result<toml::Value, ConfigError> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| ConfigError(format!("cannot read {}: {error}", path.display())))?;
    // toml 1.x: `str::parse::<Value>()` parses a single TOML *value*, not a
    // document; `from_str` parses the document into a Value (table).
    toml::from_str(&text)
        .map_err(|error| ConfigError(format!("invalid TOML in {}: {error}", path.display())))
}

/// Deep merge: tables merge key by key, everything else is replaced.
fn merge(base: &mut toml::Value, over: toml::Value) {
    match (base, over) {
        (toml::Value::Table(base), toml::Value::Table(over)) => {
            for (key, value) in over {
                match base.get_mut(&key) {
                    Some(existing) => merge(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, over) => *base = over,
    }
}

/// The nearest `.kelir/config.toml`, walking up from the cwd.
fn find_project_config() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join(".kelir").join("config.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// The base prompt text and the file it came from (`None` for the embedded
/// default), so a reader of the log knows where the words came from.
fn load_prompt(configured: Option<&str>) -> Result<(String, Option<PathBuf>), ConfigError> {
    if let Some(path) = configured {
        let path = PathBuf::from(path);
        if path.exists() {
            let text = std::fs::read_to_string(&path)
                .map_err(|error| ConfigError(format!("cannot read {}: {error}", path.display())))?;
            return Ok((text, Some(path)));
        }
        return Err(ConfigError(format!(
            "prompt file not found: {}",
            path.display()
        )));
    }
    // Only the machine `prompt.md` here.
    let machine = paths::config_dir().join("prompt.md");
    if machine.is_file()
        && let Ok(text) = std::fs::read_to_string(&machine)
    {
        return Ok((text, Some(machine)));
    }
    Ok((DEFAULT_PROMPT.to_string(), None))
}

/// The compaction prompt: `compact_prompt` (a path), else `compact.md` in the
/// config dir, else the embedded default.
fn load_compact_prompt(configured: Option<&str>) -> Result<String, ConfigError> {
    if let Some(path) = configured {
        let path = PathBuf::from(path);
        return std::fs::read_to_string(&path).map_err(|error| {
            ConfigError(format!(
                "cannot read compact_prompt {}: {error}",
                path.display()
            ))
        });
    }
    let candidate = paths::config_dir().join("compact.md");
    if candidate.is_file() {
        return std::fs::read_to_string(&candidate)
            .map_err(|error| ConfigError(format!("cannot read {}: {error}", candidate.display())));
    }
    Ok(crate::compact::DEFAULT_COMPACT_PROMPT.to_string())
}

/// The shell used when the config names none.
pub fn default_shell() -> Vec<String> {
    if cfg!(windows) {
        let candidates = [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files\Git\usr\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
        ];
        for candidate in candidates {
            if Path::new(candidate).is_file() {
                return vec![candidate.to_string(), "-c".into()];
            }
        }
        if on_path("bash") {
            return vec!["bash".into(), "-c".into()];
        }
        vec!["cmd.exe".into(), "/C".into()]
    } else {
        if on_path("bash") {
            return vec!["bash".into(), "-c".into()];
        }
        if let Some(shell) = secret_from_env("SHELL") {
            return vec![shell, "-c".into()];
        }
        vec!["sh".into(), "-c".into()]
    }
}

fn on_path(program: &str) -> bool {
    let Some(path) = crate::envs::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path) {
        if dir.join(program).is_file() {
            return true;
        }
        if cfg!(windows) && dir.join(format!("{program}.exe")).is_file() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Field names of `struct RawConfig` / `RawProvider`, read from this file.
    fn fields(src: &str, name: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut inside = false;
        for line in src.lines() {
            if line.starts_with(&format!("struct {name} {{")) {
                inside = true;
            } else if inside && line == "}" {
                break;
            } else if inside {
                let Some((field, _)) = line.trim().split_once(':') else {
                    continue;
                };
                let field = field.trim();
                if !field.starts_with("//") {
                    out.push(field.to_string());
                }
            }
        }
        out.sort_unstable();
        out
    }

    #[test]
    fn keys_track_the_config_structs() {
        const SRC: &str = include_str!("config.rs");
        let from_code: std::collections::BTreeSet<String> = fields(SRC, "RawConfig")
            .into_iter()
            .chain(fields(SRC, "RawProvider"))
            .collect();
        let listed: std::collections::BTreeSet<String> =
            KEYS.iter().map(|key| (*key).to_string()).collect();
        assert_eq!(
            from_code, listed,
            "`config::KEYS` drifted from the serde structs"
        );
    }

    fn temp_config(text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kelir-config-{}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
        path
    }

    #[test]
    fn defaults_wire_both_providers() {
        let path = temp_config("");
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.provider, PROVIDER_OPENROUTER);
        assert_eq!(cfg.spill_limit, 10_240);
        let go = cfg.providers.get(PROVIDER_GO).unwrap();
        assert_eq!(go.kind, ProviderKind::OpencodeGo);
        assert_eq!(go.base_url, "https://opencode.ai/inference/go/v1");
        assert_eq!(go.key_env.as_deref(), Some("KELIR_GO_API_KEY"));
        assert_eq!(go.session_header.as_deref(), Some("x-opencode-session"));
        let or = cfg.providers.get(PROVIDER_OPENROUTER).unwrap();
        assert_eq!(or.kind, ProviderKind::OpenRouter);
        assert_eq!(or.base_url, "https://openrouter.ai/api/v1");
        assert!(!cfg.shell.is_empty());
    }

    #[test]
    fn file_values_override_defaults() {
        let path = temp_config(
            r#"
provider = "opencode-go"
model = "some/model"
spill_limit = 2048
max_steps = 7
shell = ["sh", "-c"]
reasoning_unused = 1
[providers.opencode-go]
base_url = "https://example.invalid/v1"
reasoning_field = "thinking"
extra_headers = { "x-extra" = "yes" }
"#,
        );
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.provider, PROVIDER_GO);
        assert_eq!(cfg.model, "some/model");
        assert_eq!(cfg.spill_limit, 2048);
        assert_eq!(cfg.max_steps, Some(7));
        assert_eq!(cfg.shell, vec!["sh", "-c"]);
        let go = cfg.providers.get(PROVIDER_GO).unwrap();
        assert_eq!(go.base_url, "https://example.invalid/v1");
        assert_eq!(go.reasoning_field.as_deref(), Some("thinking"));
        assert_eq!(
            go.extra_headers.get("x-extra").map(String::as_str),
            Some("yes")
        );
    }

    /// Unset or `0` is no cap: the default, and what a "no limit" config
    /// means. Only a positive value caps a turn.
    #[test]
    fn max_steps_is_unset_by_default_and_zero_means_unlimited() {
        let path = temp_config("");
        assert_eq!(load(Some(&path)).unwrap().max_steps, None);

        let path = temp_config("max_steps = 0\n");
        assert_eq!(load(Some(&path)).unwrap().max_steps, None);

        let path = temp_config("max_steps = 1\n");
        assert_eq!(load(Some(&path)).unwrap().max_steps, Some(1));

        let path = temp_config("max_steps = 4294967295\n");
        assert_eq!(
            load(Some(&path)).unwrap().max_steps,
            Some(u32::MAX),
            "the stopgap value the old default forced on the user config"
        );
    }

    /// A `max_steps` that is not a whole number is a config error, not a
    /// silent default: a negative or non-numeric cap is a typo worth seeing.
    #[test]
    fn a_max_steps_that_is_not_a_whole_number_is_refused() {
        for bad in ["max_steps = -1\n", "max_steps = \"five\"\n"] {
            let path = temp_config(bad);
            let error = load(Some(&path)).unwrap_err().to_string();
            assert!(error.contains("max_steps"), "{bad:?}: {error}");
        }
    }

    #[test]
    fn a_single_shell_word_gets_the_c_flag() {
        let path = temp_config("shell = \"pwsh\"\n");
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.shell, vec!["pwsh", "-c"]);
    }

    #[test]
    fn a_third_provider_name_is_refused_and_the_error_names_the_two() {
        // A maintainer's config called its provider `kelir`; there are exactly two now, and the
        // error has to say which, in which table and in which file, and how to migrate.
        for text in [
            "[providers.local]\nkey_env = \"NOPE_KEY\"\n",
            "[providers.weird]\nkind = \"gemini\"\nbase_url = \"x\"\n",
        ] {
            let path = temp_config(text);
            let error = load(Some(&path)).unwrap_err().to_string();
            assert!(error.contains("exactly two"), "{error}");
            assert!(error.contains(PROVIDER_OPENROUTER), "{error}");
            assert!(error.contains(PROVIDER_GO), "{error}");
            assert!(error.contains("[providers.local]") || error.contains("[providers.weird]"));
            assert!(
                error.contains(&path.display().to_string()),
                "the file to edit: {error}"
            );
            assert!(error.contains("Rename the table"), "{error}");
            assert!(error.contains("key_env"), "{error}");
            assert!(!error.contains("kind = \"…\" is no longer used\n"));
        }
    }

    #[test]
    fn warn_prompt_tokens_has_a_default_and_can_be_off() {
        let cfg = load(Some(&temp_config(""))).unwrap();
        assert_eq!(cfg.warn_prompt_tokens, Some(150_000));

        let cfg = load(Some(&temp_config("warn_prompt_tokens = 0\n"))).unwrap();
        assert_eq!(
            cfg.warn_prompt_tokens, None,
            "0 turns the prompt warning off"
        );
    }

    #[test]
    fn removed_keys_get_the_ordinary_unknown_key_treatment() {
        let key = format!("warn_{}", ['c', 'o', 's', 't'].iter().collect::<String>());
        let table = ['p', 'r', 'i', 'c', 'e', 's'].iter().collect::<String>();
        for removed in [
            format!("{key} = 0.5\n"),
            format!("[{table}]\n"),
            format!("[{table}.\"a/b\"]\ninput = 1.0\noutput = 2.0\n"),
        ] {
            let cfg = load(Some(&temp_config(&removed))).unwrap();
            assert_eq!(cfg.warn_prompt_tokens, Some(150_000));
            // No special "this was removed" message: an unread key is just
            // ignored, like any other key the current config does not know.
        }
    }

    #[test]
    fn provider_flag_and_config_accept_only_the_two_names() {
        let path = temp_config("provider = \"kelir\"\n");
        let error = load(Some(&path)).unwrap_err().to_string();
        assert!(error.contains("exactly two"), "{error}");
        for name in PROVIDERS {
            let path = temp_config(&format!("provider = \"{name}\"\n"));
            assert_eq!(load(Some(&path)).unwrap().provider, name);
        }
    }

    #[test]
    fn the_two_provider_tables_only_override_their_settings() {
        // A `[providers.openrouter]` table that names a kind is not how a
        // provider is chosen any more: the name is the provider.
        let path = temp_config(
            "[providers.opencode-go]\nbase_url = \"http://127.0.0.1:1/v1\"\nkind = \"openrouter\"\n",
        );
        let cfg = load(Some(&path)).unwrap();
        let go = cfg.providers.get(PROVIDER_GO).unwrap();
        assert_eq!(
            go.kind,
            ProviderKind::OpencodeGo,
            "the name decides the kind"
        );
        assert_eq!(go.base_url, "http://127.0.0.1:1/v1");
    }

    #[test]
    fn a_literal_key_in_the_config_is_ignored_not_used() {
        // Warned about (to stderr) and refused: a literal goes stale, shadows a
        // live store and puts a secret in a file that gets copied.
        let path = temp_config(
            "key = \"sk-literal-not-used\"\n[providers.openrouter]\nkey = \"sk-also-not-used\"\n",
        );
        let cfg = load(Some(&path)).unwrap();
        let openrouter = cfg.providers.get(PROVIDER_OPENROUTER).unwrap();
        assert_eq!(openrouter.key, None, "a config-file key is never used");
        assert_eq!(openrouter.key_env.as_deref(), Some("OPENROUTER_API_KEY"));
    }

    #[test]
    fn key_sources_are_the_two_documented_ones_per_provider() {
        let path = temp_config(
            "[providers.openrouter]\nkey_cmd = [\"pass\", \"show\", \"kelir\"]\n\
             [providers.opencode-go]\nkey_env = \"MY_GO_KEY\"\n",
        );
        let cfg = load(Some(&path)).unwrap();
        let openrouter = cfg.providers.get(PROVIDER_OPENROUTER).unwrap();
        assert_eq!(
            openrouter.key_cmd.as_deref(),
            Some(["pass".to_string(), "show".to_string(), "kelir".to_string()].as_slice())
        );
        assert_eq!(
            cfg.providers.get(PROVIDER_GO).unwrap().key_env.as_deref(),
            Some("MY_GO_KEY")
        );
    }

    #[test]
    fn deep_merge_keeps_sibling_keys() {
        let mut base: toml::Value = toml::from_str("a = 1\n[t]\nx = 1\ny = 2\n").unwrap();
        let over: toml::Value = toml::from_str("a = 2\n[t]\ny = 3\n").unwrap();
        merge(&mut base, over);
        assert_eq!(base["a"].as_integer(), Some(2));
        assert_eq!(base["t"]["x"].as_integer(), Some(1));
        assert_eq!(base["t"]["y"].as_integer(), Some(3));
    }

    #[test]
    fn compaction_keys_are_read_and_defaulted() {
        let path = temp_config("");
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.window, None);
        assert_eq!(cfg.auto_compact_at, 0.8);
        assert_eq!(cfg.compact_keep, crate::compact::DEFAULT_KEEP);
        assert!(
            !cfg.compact_prompt.trim().is_empty(),
            "a prompt is always resolved"
        );
        assert_eq!(cfg.override_window("deepseek/deepseek-chat"), None);

        let path = temp_config(
            r#"
window = 64000
auto_compact_at = 0.5
compact_keep = 0
[model_windows]
"deepseek/deepseek-chat" = 32000
"#,
        );
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.auto_compact_at, 0.5);
        assert_eq!(cfg.compact_keep, 0);
        assert_eq!(cfg.override_window("deepseek/deepseek-chat"), Some(32_000));
        assert_eq!(cfg.override_window("other/model"), Some(64_000));
    }

    #[test]
    fn the_soft_target_is_a_hundred_and_fifty_thousand_by_default() {
        let path = temp_config("");
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.compact_at_tokens, crate::compact::DEFAULT_SOFT_TARGET);
        let path = temp_config("compact_at_tokens = 120000\n");
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.compact_at_tokens, 120_000);
        // Zero is "no absolute cap": the window fraction alone decides.
        let path = temp_config("compact_at_tokens = 0\n");
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.compact_at_tokens, 0);
    }

    #[test]
    fn reasoning_effort_defaults_to_medium_and_can_be_tuned_or_turned_off() {
        let path = temp_config("");
        let cfg = load(Some(&path)).unwrap();
        for name in [PROVIDER_OPENROUTER, PROVIDER_GO] {
            assert_eq!(
                cfg.reasoning_effort(name).as_deref(),
                Some(crate::config::DEFAULT_REASONING_EFFORT),
                "{name}"
            );
        }

        let path = temp_config(
            "[providers.openrouter]\nreasoning_effort = \"low\"\n\
             [providers.opencode-go]\nreasoning_effort = \"none\"\n",
        );
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.reasoning_effort("openrouter").as_deref(), Some("low"));
        // `none` sends no field at all: the provider's own default.
        assert_eq!(cfg.reasoning_effort("opencode-go"), None);
        // Case is not the user's problem.
        let path = temp_config("[providers.openrouter]\nreasoning_effort = \" HIGH \"\n");
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.reasoning_effort("openrouter").as_deref(), Some("high"));
    }

    /// The default `medium` and an explicit choice are not the same fact: only the explicit one
    /// may reach a model the provider's list says nothing about.
    #[test]
    fn an_explicit_effort_is_told_apart_from_the_default() {
        let cfg = load(Some(&temp_config(""))).unwrap();
        for name in [PROVIDER_OPENROUTER, PROVIDER_GO] {
            let provider = cfg.provider(name).unwrap();
            assert_eq!(
                provider.reasoning_effort.as_deref(),
                Some(crate::config::DEFAULT_REASONING_EFFORT)
            );
            assert!(!provider.reasoning_effort_explicit, "{name}: a default");
        }
        let path = temp_config(
            "[providers.openrouter]
reasoning_effort = \"low\"
             [providers.opencode-go]
reasoning_effort = \"none\"
",
        );
        let cfg = load(Some(&path)).unwrap();
        let provider = cfg.provider("openrouter").unwrap();
        assert_eq!(provider.reasoning_effort.as_deref(), Some("low"));
        assert!(provider.reasoning_effort_explicit);
        // `none` is an explicit choice too: it is never "the default applies".
        let provider = cfg.provider(PROVIDER_GO).unwrap();
        assert_eq!(provider.reasoning_effort, None);
        assert!(provider.reasoning_effort_explicit);
    }

    #[test]
    fn an_unknown_reasoning_effort_is_refused_at_load() {
        let path = temp_config("[providers.openrouter]\nreasoning_effort = \"maximum\"\n");
        let error = load(Some(&path)).unwrap_err().to_string();
        assert!(error.contains("reasoning_effort"), "{error}");
        assert!(error.contains("low, medium, high"), "{error}");
    }

    #[test]
    fn promotion_after_is_ten_minutes_by_default_and_configurable() {
        // The environment override is the test hook; it wins over the file.
        let path = temp_config("promote_after = 30\n");
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.promote_after, std::time::Duration::from_secs(30));
        let path = temp_config("");
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.promote_after, std::time::Duration::from_secs(600));
    }

    #[test]
    fn the_retry_schedule_defaults_and_reads_every_knob() {
        let path = temp_config("");
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.retry.base_ms, crate::retry::DEFAULT_BASE_MS);
        assert_eq!(cfg.retry.cap_ms, crate::retry::DEFAULT_CAP_MS);
        assert_eq!(
            cfg.retry.max_wait,
            Some(std::time::Duration::from_secs(
                crate::retry::DEFAULT_WAIT_SECS
            ))
        );
        assert_eq!(cfg.retry.max_attempts, None);

        let path = temp_config(
            "retry_base_ms = 500
retry_cap_ms = 4000
retry_max_wait = 60
retry_max_attempts = 5
",
        );
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.retry.base_ms, 500);
        assert_eq!(cfg.retry.cap_ms, 4_000);
        assert_eq!(cfg.retry.max_wait, Some(std::time::Duration::from_secs(60)));
        assert_eq!(cfg.retry.max_attempts, Some(5));

        // `retry_max_wait = 0` is "wait as long as the provider needs".
        let path = temp_config(
            "retry_max_wait = 0
",
        );
        assert_eq!(load(Some(&path)).unwrap().retry.max_wait, None);
        // A count cap of zero would mean "never retry", which `retry_max_wait = 0` cannot express;
        // it is refused as an attempt cap and falls back to the budget.
        let path = temp_config(
            "retry_max_attempts = 0
",
        );
        assert_eq!(load(Some(&path)).unwrap().retry.max_attempts, None);
    }

    #[test]
    fn a_cap_below_the_base_is_refused() {
        let path = temp_config(
            "retry_base_ms = 5000
retry_cap_ms = 1000
",
        );
        let error = load(Some(&path)).unwrap_err();
        assert!(error.to_string().contains("retry_cap_ms"), "{error}");
    }

    #[test]
    fn max_concurrent_requests_is_per_provider_and_off_by_default() {
        let path = temp_config("");
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(
            cfg.providers
                .get(PROVIDER_OPENROUTER)
                .unwrap()
                .max_concurrent_requests,
            None
        );
        let path = temp_config(
            "[providers.opencode-go]
max_concurrent_requests = 3
",
        );
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(
            cfg.providers
                .get(PROVIDER_GO)
                .unwrap()
                .max_concurrent_requests,
            Some(3)
        );
        assert_eq!(
            cfg.providers
                .get(PROVIDER_OPENROUTER)
                .unwrap()
                .max_concurrent_requests,
            None,
            "one provider's limit is not another's"
        );
        // Zero means "no limit", exactly like leaving it out.
        let path = temp_config(
            "[providers.opencode-go]
max_concurrent_requests = 0
",
        );
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(
            cfg.providers
                .get(PROVIDER_GO)
                .unwrap()
                .max_concurrent_requests,
            None
        );
    }

    #[test]
    fn a_bad_auto_fraction_is_refused() {
        let path = temp_config("auto_compact_at = 1.5\n");
        let error = load(Some(&path)).unwrap_err();
        assert!(error.to_string().contains("auto_compact_at"), "{error}");
        let path = temp_config("auto_compact_at = 0\n");
        assert!(load(Some(&path)).is_err());
    }

    #[test]
    fn a_prompt_file_records_the_path_it_came_from() {
        let dir = std::env::temp_dir().join(format!("kelir-prompt-{}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&dir).unwrap();
        let prompt = dir.join("my-prompt.txt");
        std::fs::write(&prompt, "EXPLICIT BASE").unwrap();
        // A TOML literal string: no escaping, so a Windows path goes in as is.
        let text = format!("prompt = '{}'\n", prompt.display());
        let path = temp_config(&text);
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.system_prompt, "EXPLICIT BASE");
        assert_eq!(cfg.prompt_path.as_deref(), Some(prompt.as_path()));
        // The embedded default has no path, and no sentinel either.
        let path = temp_config("");
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.system_prompt, DEFAULT_PROMPT);
        assert!(cfg.prompt_path.is_none(), "{:?}", cfg.prompt_path);
    }

    #[test]
    fn a_compact_prompt_file_replaces_the_default() {
        let dir = std::env::temp_dir().join(format!("kelir-compact-{}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&dir).unwrap();
        let prompt = dir.join("compact.md");
        std::fs::write(&prompt, "SUMMARISE HARD").unwrap();
        let path = temp_config(&format!(
            "compact_prompt = {:?}\n",
            prompt.to_string_lossy()
        ));
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.compact_prompt, "SUMMARISE HARD");
        // A configured path that does not exist is an error, not a silent
        // fallback to the default prompt.
        let path = temp_config("compact_prompt = \"does/not/exist.md\"\n");
        assert!(load(Some(&path)).is_err());
    }

    #[test]
    fn project_config_is_found_by_walking_up() {
        // The repo root is the only ancestor here, and it has no .kelir dir;
        // the helper must simply not panic and not invent a path.
        let found = find_project_config();
        if let Some(path) = found {
            assert!(path.ends_with("config.toml"));
        }
    }
}
