//! Resolution of raw config into typed runtime values.
use crate::config::{raw::*, types::*};
use crate::paths;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Validate and resolve `[mcp.servers.*]` into the shape the broker uses.
pub(super) fn mcp_servers(
    raw: &RawMcp,
) -> Result<BTreeMap<String, crate::features::mcp::config::ServerConfig>, ConfigError> {
    crate::features::mcp::config::from_raw(&raw.servers).map_err(ConfigError)
}

/// The `[notify]` table: event -> command list, plus terminal and rate limit.
/// The hook names are kept as a map; every other key is a typed field, so a
/// typo is the ordinary unknown-key error from serde.
pub(super) fn notify(raw: &RawNotify) -> Result<crate::features::notify::Config, ConfigError> {
    if let Some(kind) = &raw.terminal_kind
        && !kind.is_empty()
        && kind != "osc9"
        && kind != "osc777"
    {
        return Err(ConfigError(
            "notify.terminal_kind must be `osc9` or `osc777`".into(),
        ));
    }
    let mut hooks = BTreeMap::new();
    for (kind, entry) in &raw.hooks {
        if !crate::features::notify::KINDS.contains(&kind.as_str()) {
            return Err(ConfigError(format!(
                "unknown field `{kind}` in `[notify]`, expected one of {}",
                crate::features::notify::KINDS
                    .iter()
                    .map(|name| format!("`{name}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
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
        hooks.insert(kind.clone(), commands);
    }
    Ok(crate::features::notify::Config {
        hooks,
        terminal: raw.terminal,
        terminal_kind: raw.terminal_kind.clone().unwrap_or_default(),
        rate_limit_secs: raw.rate_limit.unwrap_or_default(),
        title: raw.title.clone().unwrap_or_default(),
    })
}

/// The retry schedule and budget. A wrong value is a config error rather than a
/// silent fallback: a person who writes a `retry` key means it.
pub(super) fn retry_policy(raw: &RawConfig) -> Result<crate::provider::retry::Policy, ConfigError> {
    let base_ms = raw
        .retry
        .base_ms
        .unwrap_or(crate::provider::retry::DEFAULT_BASE_MS);
    let cap_ms = raw
        .retry
        .cap_ms
        .unwrap_or(crate::provider::retry::DEFAULT_CAP_MS);
    if cap_ms < base_ms {
        return Err(ConfigError(format!(
            "retry.cap_ms ({cap_ms}) must be at least retry.base_ms ({base_ms})"
        )));
    }
    let max_wait = match raw.retry.max_wait {
        // `0` is the documented "wait as long as the provider needs".
        Some(0) => None,
        Some(seconds) => Some(std::time::Duration::from_secs(seconds)),
        None => Some(std::time::Duration::from_secs(
            crate::provider::retry::DEFAULT_WAIT_SECS,
        )),
    };
    Ok(crate::provider::retry::Policy {
        base_ms,
        cap_ms,
        max_wait,
        max_attempts: raw.retry.max_attempts.filter(|attempts| *attempts > 0),
    })
}

/// The effort kelir asks for when the config says nothing: enough thinking to
/// plan, not enough to re-derive the plan every step.
pub const DEFAULT_REASONING_EFFORT: &str = "medium";

/// The values `reasoning_effort` accepts.
pub const REASONING_EFFORTS: [&str; 3] = ["low", "medium", "high"];

/// Resolve one provider's `reasoning_effort`:
pub(super) fn reasoning_effort(
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
pub(super) fn step_cap(configured: Option<u32>) -> Option<u32> {
    configured.filter(|steps| *steps > 0)
}

/// Seconds between `config.toml` and `KELIR_PROMOTE_AFTER`. Zero disables
/// promotion: a long call then runs to completion, as it did before.
pub(super) fn promote_after(configured: Option<u64>) -> Result<std::time::Duration, ConfigError> {
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
    /// else [`crate::core::compact`]'s own default for the loop (`medium`), else nothing.
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
pub(super) fn expand_home(path: &str) -> PathBuf {
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

pub(super) fn secret_from_env(name: &str) -> Option<String> {
    crate::envs::named(name)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn overlay(base: RawProvider, user: RawProvider) -> RawProvider {
    RawProvider {
        base_url: user.base_url.or(base.base_url),
        key_env: user.key_env.or(base.key_env),
        key_cmd: user.key_cmd.or(base.key_cmd),
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

pub(super) fn read_toml(path: &Path) -> Result<toml::Value, ConfigError> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| ConfigError(format!("cannot read {}: {error}", path.display())))?;
    // toml 1.x: `str::parse::<Value>()` parses a single TOML *value*, not a
    // document; `from_str` parses the document into a Value (table).
    toml::from_str::<toml::Value>(&text).map_err(|error| {
        let message = error.message().to_string();
        let (line, column) = error
            .span()
            .map(|span| {
                let before = &text[..span.start.min(text.len())];
                (
                    before.matches('\n').count() + 1,
                    before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1,
                )
            })
            .unwrap_or((0, 0));
        ConfigError(format!(
            "invalid TOML in {} at line {line}, column {column}: {message}",
            path.display()
        ))
    })
}

/// serde quotes the offending value in type errors. A config can hold secrets
/// (an MCP server's `env`), and this text reaches the model and the log: keep
/// the field path and the expectation, never the value.
pub(super) fn safe_de_error(error: &toml::de::Error) -> String {
    let text = error.to_string();
    let mut lines = text.lines();
    let message = lines.next().unwrap_or("invalid config");
    let mut out = String::new();
    for (index, part) in message.split(", expected ").enumerate() {
        if index > 0 {
            out.push_str(", expected ");
        }
        if index == 0 && (part.starts_with("invalid type: ") || part.starts_with("invalid value: "))
        {
            out.push_str(part.split(':').next().unwrap_or("invalid value"));
            out.push_str(" (value hidden)");
        } else {
            out.push_str(part);
        }
    }
    for line in lines {
        out.push('\n');
        out.push_str(line);
    }
    out
}

/// Deep merge: tables merge key by key, everything else is replaced.
pub(super) fn merge(base: &mut toml::Value, over: toml::Value) {
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
pub(super) fn find_project_config(cwd: &Path) -> Option<PathBuf> {
    let mut current = Some(cwd.to_path_buf());
    while let Some(dir) = current {
        let candidate = dir.join(".kelir").join("config.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        current = dir.parent().map(Path::to_path_buf);
    }
    None
}

/// The base prompt text and the file it came from (`None` for the embedded
/// default), so a reader of the log knows where the words came from.
pub(super) fn load_prompt(
    configured: Option<&str>,
) -> Result<(String, Option<PathBuf>), ConfigError> {
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

/// The compaction prompt: `[compaction] prompt` (a path), else `compact.md` in the
/// config dir, else the embedded default.
pub(super) fn load_compact_prompt(configured: Option<&str>) -> Result<String, ConfigError> {
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
    Ok(crate::core::compact::DEFAULT_COMPACT_PROMPT.to_string())
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
        // `C:\Windows\System32\bash.exe` is the WSL launcher, not a POSIX shell:
        // it translates a Windows script path and fails before running it. Only
        // take a bash from PATH when a real Git bash.exe is found there.
        if let Some(bash) = git_bash_on_path() {
            return vec![bash, "-c".into()];
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

/// Resolve a configured shell. A bare `bash` on Windows is the System32 WSL
/// stub on most machines; use a real Git bash.exe when one is available and
/// never start the stub, which cannot run a cmd.sh.
pub(super) fn resolve_shell(spec: Option<ShellSpec>) -> Vec<String> {
    match spec {
        Some(ShellSpec::One(shell)) => {
            if cfg!(windows) && (shell == "bash" || shell == "bash.exe") {
                // `default_shell()` always yields at least one shell on
                // Windows (cmd.exe), so no error is possible here.
                let resolved = git_bash_on_path()
                    .or_else(|| default_shell().into_iter().next())
                    .expect("a Windows default shell is always present");
                vec![resolved, "-c".into()]
            } else {
                vec![shell, "-c".into()]
            }
        }
        Some(ShellSpec::Many(argv)) if !argv.is_empty() => argv,
        _ => default_shell(),
    }
}

/// The first real Git `bash.exe` on PATH. A bare `bash` on Windows is usually
/// the System32 WSL stub, which cannot run a cmd.sh: it must never be chosen.
/// A Git bash has the companion `sh.exe` next to it, which the System32 WSL
/// stub does not; the check is a property of the file, not of the current
/// machine, so `git_bash_in` is testable with any PATH.
pub(super) fn git_bash_on_path() -> Option<String> {
    let path = crate::envs::var_os("PATH")?;
    git_bash_in(&path)
}

fn git_bash_in(path: &std::ffi::OsStr) -> Option<String> {
    for dir in std::env::split_paths(path) {
        let exe = dir.join("bash.exe");
        if !exe.is_file() {
            continue;
        }
        // Git for Windows installs `sh.exe` beside `bash.exe`; the WSL stub in
        // System32 does not. This distinguishes them on the real machine and
        // in a test fixture.
        if !dir.join("sh.exe").is_file() {
            continue;
        }
        return Some(exe.to_string_lossy().into_owned());
    }
    None
}

pub(super) fn on_path(program: &str) -> bool {
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

    fn empty_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn a_bash_without_its_companion_sh_is_not_a_git_shell() {
        let stub = empty_dir();
        std::fs::write(stub.path().join("bash.exe"), b"").unwrap();
        let git = empty_dir();
        std::fs::write(git.path().join("bash.exe"), b"").unwrap();
        std::fs::write(git.path().join("sh.exe"), b"").unwrap();
        let path = std::env::join_paths([stub.path().as_os_str(), git.path().as_os_str()]).unwrap();
        assert_eq!(
            git_bash_in(&path).as_deref(),
            Some(git.path().join("bash.exe").to_str().unwrap()),
            "the WSL stub has no sh.exe beside it"
        );
        let only_stub = std::env::join_paths([stub.path().as_os_str()]).unwrap();
        assert_eq!(git_bash_in(&only_stub), None);
    }
}
