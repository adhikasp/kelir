//! `config.toml`: machine config, then the project's `.kelir/config.toml`.
//!
//! CLI flags override the file; the environment supplies the keys and can
//! point the provider at a stand-in base URL (`KELIR_BASE_URL`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::paths;

pub const DEFAULT_PROMPT: &str = "\
You are kelir, a small autonomous coding-agent harness. You work alone: there \
is no human to ask, so never ask a question or wait for approval. Reach for \
`bash` to read, search and run things, and `edit` to change a file. Prefer \
small steps you can verify, and say what you did when you are done.";

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
}

#[derive(Debug, Clone)]
pub struct Config {
    pub provider: String,
    pub model: String,
    /// argv prefix; the command is appended (so `bash -c` or `pwsh -Command`).
    pub shell: Vec<String>,
    pub spill_limit: usize,
    pub max_steps: u32,
    pub cache_ttl: Option<String>,
    pub system_prompt: String,
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Context window override for every model (`window`), when set.
    pub window: Option<u64>,
    /// Per-model window overrides (`[model_windows]`).
    pub model_windows: BTreeMap<String, u64>,
    /// Fraction of the window at which compaction triggers automatically.
    pub auto_compact_at: f64,
    /// The summary prompt (`compact_prompt`/`compact.md`, else embedded).
    pub compact_prompt: String,
    /// Messages kept verbatim after a compaction.
    pub compact_keep: usize,
    /// A bash call still running after this long is promoted to a background
    /// job instead of being killed (`config.toml` `promote_after`, seconds;
    /// `KELIR_PROMOTE_AFTER` overrides it; zero disables promotion).
    pub promote_after: std::time::Duration,
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
    promote_after: Option<u64>,
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
            },
        );
    }

    let system_prompt = load_prompt(raw.prompt.as_deref())?;
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
            .unwrap_or_else(|| "deepseek/deepseek-v4.1-flash".into()),
        shell,
        spill_limit: raw.spill_limit.unwrap_or(10_240),
        max_steps: raw.max_steps.unwrap_or(100),
        cache_ttl: raw.cache_ttl,
        system_prompt,
        providers,
        window: raw.window,
        model_windows: raw.model_windows,
        auto_compact_at,
        compact_prompt,
        compact_keep: raw.compact_keep.unwrap_or(crate::compact::DEFAULT_KEEP),
        promote_after: promote_after(raw.promote_after)?,
    })
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

    /// The model's context window: `[model_windows]`, then `window`, then the
    /// built-in per-model table, then 128 000.
    pub fn window(&self, model: &str) -> u64 {
        crate::compact::window_for(model, &self.model_windows, self.window)
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

fn load_prompt(configured: Option<&str>) -> Result<String, ConfigError> {
    if let Some(path) = configured {
        let path = PathBuf::from(path);
        if path.exists() {
            return std::fs::read_to_string(&path)
                .map_err(|error| ConfigError(format!("cannot read {}: {error}", path.display())));
        }
        return Err(ConfigError(format!(
            "prompt file not found: {}",
            path.display()
        )));
    }
    let candidates = [
        paths::config_dir().join("prompt.md"),
        std::env::current_dir()
            .map(|dir| dir.join(".kelir").join("prompt.md"))
            .unwrap_or_default(),
    ];
    for candidate in candidates {
        if candidate.is_file()
            && let Ok(text) = std::fs::read_to_string(&candidate)
        {
            return Ok(text);
        }
    }
    Ok(DEFAULT_PROMPT.to_string())
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
        assert_eq!(cfg.max_steps, 7);
        assert_eq!(cfg.shell, vec!["sh", "-c"]);
        let go = cfg.providers.get(PROVIDER_GO).unwrap();
        assert_eq!(go.base_url, "https://example.invalid/v1");
        assert_eq!(go.reasoning_field.as_deref(), Some("thinking"));
        assert_eq!(
            go.extra_headers.get("x-extra").map(String::as_str),
            Some("yes")
        );
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
        assert_eq!(cfg.window("deepseek/deepseek-chat"), 128_000);

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
        assert_eq!(cfg.window("deepseek/deepseek-chat"), 32_000);
        assert_eq!(cfg.window("other/model"), 64_000);
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
    fn a_bad_auto_fraction_is_refused() {
        let path = temp_config("auto_compact_at = 1.5\n");
        let error = load(Some(&path)).unwrap_err();
        assert!(error.to_string().contains("auto_compact_at"), "{error}");
        let path = temp_config("auto_compact_at = 0\n");
        assert!(load(Some(&path)).is_err());
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
