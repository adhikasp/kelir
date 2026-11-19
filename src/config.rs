//! `config.toml`: machine config, then the project's `.kelir/config.toml`.
//!
//! CLI flags override the file; the environment supplies the keys and can
//! point the provider at a stand-in base URL (`KELIR_BASE_URL`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::paths;

/// The base prompt, embedded at build time (`src/prompt.md`, at most 1 KB).
///
/// It names the skills directories and carries no listing: skills are pull, so
/// the model reads a `SKILL.md` with `bash` when the task calls for one.
pub const DEFAULT_PROMPT: &str = include_str!("prompt.md");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    OpenRouter,
    OpencodeGo,
    Custom,
}

#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    pub base_url: String,
    pub key_env: Option<String>,
    pub key: Option<String>,
    pub extra_headers: BTreeMap<String, String>,
    pub reasoning_field: Option<String>,
    /// Header carrying the session id for providers that require one.
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
    /// The cap on one turn's steps: `None` means unlimited, which is the
    /// default. An explicit positive `max_steps` (or `--max-steps`) caps a
    /// turn, and reaching it *pauses* the turn rather than failing it.
    pub max_steps: Option<u32>,
    pub cache_ttl: Option<String>,
    pub system_prompt: String,
    /// The file `system_prompt` was read from (`prompt` or `prompt.md`), when
    /// it came from one: `sources[]` records it, so a log reader can tell a
    /// file's base prompt from the embedded default (F2).
    pub prompt_path: Option<PathBuf>,
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Context window override for every model (`window`), when set.
    pub window: Option<u64>,
    /// Per-model window overrides (`[model_windows]`).
    pub model_windows: BTreeMap<String, u64>,
    /// Fraction of the window at which compaction triggers automatically.
    pub auto_compact_at: f64,
    /// The summary prompt (`compact_prompt`/`compact.md`, else embedded).
    pub compact_prompt: String,
    /// Extra skills directories (`skills_dirs`), machine layer: they are named
    /// in the base prompt and searched read-only.
    pub skills_dirs: Vec<PathBuf>,
    /// Messages kept verbatim after a compaction.
    pub compact_keep: usize,
    /// A bash call still running after this long is promoted to a background
    /// job instead of being killed (`config.toml` `promote_after`, seconds;
    /// `KELIR_PROMOTE_AFTER` overrides it; zero disables promotion).
    pub promote_after: std::time::Duration,
    /// What to retry and how long to wait (`retry_base_ms`, `retry_cap_ms`,
    /// `retry_max_wait`).
    pub retry: crate::retry::Policy,
}

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
    window: Option<u64>,
    model_windows: BTreeMap<String, u64>,
    auto_compact_at: Option<f64>,
    compact_prompt: Option<String>,
    compact_keep: Option<usize>,
    skills_dirs: Vec<PathBuf>,
    promote_after: Option<u64>,
    retry_base_ms: Option<u64>,
    retry_cap_ms: Option<u64>,
    /// Total waiting per request in seconds; `0` waits as long as it takes.
    retry_max_wait: Option<u64>,
    /// A count cap inside the budget (`retry_max_attempts`), for a job that
    /// wants "three tries and out".
    retry_max_attempts: Option<u32>,
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
    kind: Option<String>,
    base_url: Option<String>,
    key_env: Option<String>,
    extra_headers: BTreeMap<String, String>,
    reasoning_field: Option<String>,
    session_header: Option<String>,
    max_concurrent_requests: Option<u32>,
}

pub const PROVIDER_OPENROUTER: &str = "openrouter";
pub const PROVIDER_GO: &str = "opencode-go";

fn builtin_provider(name: &str) -> Option<RawProvider> {
    match name {
        PROVIDER_OPENROUTER => Some(RawProvider {
            kind: Some("openrouter".into()),
            base_url: Some("https://openrouter.ai/api/v1".into()),
            key_env: Some("OPENROUTER_API_KEY".into()),
            ..RawProvider::default()
        }),
        PROVIDER_GO => Some(RawProvider {
            kind: Some("opencode-go".into()),
            base_url: Some("https://opencode.ai/inference/go/v1".into()),
            key_env: Some("KELIR_GO_API_KEY".into()),
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
        .or_else(|| std::env::var_os("KELIR_CONFIG").map(PathBuf::from))
        .filter(|p| !p.as_os_str().is_empty());

    let mut merged = toml::Value::Table(Default::default());
    match explicit {
        Some(path) => {
            let value = read_toml(&path)?;
            merge(&mut merged, value);
        }
        None => {
            let machine = paths::machine_config_path();
            if machine.exists() {
                merge(&mut merged, read_toml(&machine)?);
            }
            if let Some(project) = find_project_config() {
                merge(&mut merged, read_toml(&project)?);
            }
        }
    }

    let raw: RawConfig = merged
        .try_into()
        .map_err(|error| ConfigError(format!("invalid config.toml: {error}")))?;
    // Read the knobs that are plain numbers before anything is moved out of
    // `raw`.
    let retry = retry_policy(&raw)?;

    let shell = match raw.shell {
        Some(ShellSpec::One(shell)) => vec![shell, "-c".into()],
        Some(ShellSpec::Many(argv)) if !argv.is_empty() => argv,
        _ => default_shell(),
    };

    let mut providers = BTreeMap::new();
    let mut names: Vec<String> = vec![PROVIDER_OPENROUTER.into(), PROVIDER_GO.into()];
    for name in raw.providers.keys() {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    for name in names {
        let base = builtin_provider(&name).unwrap_or_default();
        let user = raw.providers.get(&name).cloned().unwrap_or_default();
        let entry = overlay(base, user);
        let kind = match entry.kind.as_deref() {
            Some("openrouter") => ProviderKind::OpenRouter,
            Some("opencode-go") => ProviderKind::OpencodeGo,
            Some("custom") => ProviderKind::Custom,
            Some(other) => {
                return Err(ConfigError(format!(
                    "provider `{name}`: unknown kind `{other}` (openrouter, opencode-go, custom)"
                )));
            }
            None if name == PROVIDER_OPENROUTER => ProviderKind::OpenRouter,
            None if name == PROVIDER_GO => ProviderKind::OpencodeGo,
            None => ProviderKind::Custom,
        };
        let base_url = entry
            .base_url
            .clone()
            .filter(|url| !url.is_empty())
            .ok_or_else(|| ConfigError(format!("provider `{name}`: base_url is required")))?;
        let key = entry.key_env.as_deref().and_then(secret_from_env);
        providers.insert(
            name,
            ProviderConfig {
                kind,
                base_url,
                key_env: entry.key_env,
                key,
                extra_headers: entry.extra_headers,
                reasoning_field: entry.reasoning_field,
                session_header: entry.session_header,
                max_concurrent_requests: entry.max_concurrent_requests.filter(|slots| *slots > 0),
            },
        );
    }

    // The base prompt and the file it came from (`None` for the embedded
    // default), so `sources[]` records the real path (F2).
    let (system_prompt, prompt_path) = load_prompt(raw.prompt.as_deref())?;
    let compact_prompt = load_compact_prompt(raw.compact_prompt.as_deref())?;
    let auto_compact_at = raw.auto_compact_at.unwrap_or(crate::compact::AUTO_FRACTION);
    if !(0.0..=1.0).contains(&auto_compact_at) || auto_compact_at == 0.0 {
        return Err(ConfigError(format!(
            "auto_compact_at must be between 0 and 1 (got {auto_compact_at})"
        )));
    }

    Ok(Config {
        provider: raw.provider.unwrap_or_else(|| PROVIDER_OPENROUTER.into()),
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
        promote_after: promote_after(raw.promote_after)?,
        retry,
    })
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

/// `max_steps`: unset or `0` means no cap at all (the default: a fully
/// autonomous agent is not stopped by a counter), a positive value caps one
/// turn. `0` is accepted as "unlimited" rather than refused, because a config
/// that means "no limit" should not be able to break a session. A value that is
/// not a positive whole number (`-1`, `"lots"`) is refused while parsing.
fn step_cap(configured: Option<u32>) -> Option<u32> {
    configured.filter(|steps| *steps > 0)
}

/// Seconds between `config.toml` and `KELIR_PROMOTE_AFTER`. Zero disables
/// promotion: a long call then runs to completion, as it did before M4.
fn promote_after(configured: Option<u64>) -> Result<std::time::Duration, ConfigError> {
    let seconds = match std::env::var_os("KELIR_PROMOTE_AFTER") {
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
            .ok_or_else(|| ConfigError(format!("unknown provider `{name}`")))?;
        // A stand-in for either provider (the mock server in tests, a proxy).
        if let Some(url) = std::env::var_os("KELIR_BASE_URL")
            .map(|url| url.to_string_lossy().trim().to_string())
            .filter(|url| !url.is_empty())
        {
            provider.base_url = url;
        }
        Ok(provider)
    }

    /// The window a configuration override names, if any: `[model_windows]`
    /// for this model, else `window`. What the provider says is
    /// [`crate::models::resolve`]; this is only the part a person set.
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
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn overlay(base: RawProvider, user: RawProvider) -> RawProvider {
    RawProvider {
        kind: user.kind.or(base.kind),
        base_url: user.base_url.or(base.base_url),
        key_env: user.key_env.or(base.key_env),
        extra_headers: {
            let mut headers = base.extra_headers;
            headers.extend(user.extra_headers);
            headers
        },
        reasoning_field: user.reasoning_field.or(base.reasoning_field),
        session_header: user.session_header.or(base.session_header),
        max_concurrent_requests: user
            .max_concurrent_requests
            .or(base.max_concurrent_requests),
    }
}

fn read_toml(path: &Path) -> Result<toml::Value, ConfigError> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| ConfigError(format!("cannot read {}: {error}", path.display())))?;
    text.parse::<toml::Value>()
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
    // Only the machine `prompt.md` here. The project's `.kelir/prompt.md` is
    // *not* looked up from the process cwd: a session's cwd is `--cwd`, which
    // the config does not know, and the render already reads
    // `<cwd>/.kelir/prompt.md` first (M5-A fixes F2, second half). Reading it
    // here would both miss `--cwd` and record a path the text did not come
    // from.
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
    let Some(path) = std::env::var_os("PATH") else {
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

    fn temp_config(text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kelir-config-{}", ulid::Ulid::new()));
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
    fn custom_provider_needs_a_base_url() {
        let path = temp_config("[providers.local]\nkey_env = \"NOPE_KEY\"\n");
        let error = load(Some(&path)).unwrap_err();
        assert!(error.to_string().contains("base_url"), "{error}");
    }

    #[test]
    fn unknown_provider_kind_is_refused() {
        let path = temp_config("[providers.weird]\nkind = \"gemini\"\nbase_url = \"x\"\n");
        let error = load(Some(&path)).unwrap_err();
        assert!(error.to_string().contains("unknown kind"), "{error}");
    }

    #[test]
    fn deep_merge_keeps_sibling_keys() {
        let mut base: toml::Value = "a = 1\n[t]\nx = 1\ny = 2\n".parse().unwrap();
        let over: toml::Value = "a = 2\n[t]\ny = 3\n".parse().unwrap();
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
        // A count cap of zero would mean "never retry", which `retry_max_wait
        // = 0` cannot express; it is refused as an attempt cap and falls back
        // to the budget.
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
        let dir = std::env::temp_dir().join(format!("kelir-prompt-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let prompt = dir.join("my-prompt.txt");
        std::fs::write(&prompt, "EXPLICIT BASE").unwrap();
        // A TOML literal string: no escaping, so a Windows path goes in as is.
        let text = format!("prompt = '{}'\n", prompt.display());
        let path = temp_config(&text);
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.system_prompt, "EXPLICIT BASE");
        assert_eq!(cfg.prompt_path.as_deref(), Some(prompt.as_path()));
        // The embedded default has no path, and no sentinel either (F2, F2b).
        let path = temp_config("");
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.system_prompt, DEFAULT_PROMPT);
        assert!(cfg.prompt_path.is_none(), "{:?}", cfg.prompt_path);
    }

    #[test]
    fn a_compact_prompt_file_replaces_the_default() {
        let dir = std::env::temp_dir().join(format!("kelir-compact-{}", ulid::Ulid::new()));
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
