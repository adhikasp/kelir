//! Config discovery, file loading and merge.
use crate::config::resolve::*;
use crate::config::{raw::*, types::*};
use crate::paths;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Load the config. `explicit` (CLI flag) or `KELIR_CONFIG` replace the search.
pub fn load(explicit: Option<&Path>) -> Result<Config, ConfigError> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    load_for_cwd(explicit, &cwd)
}

/// Load with an explicit working directory: the MCP broker uses the session's
/// cwd, which can differ from the process's, to find the project layer.
pub fn load_for_cwd(explicit: Option<&Path>, cwd: &Path) -> Result<Config, ConfigError> {
    let explicit = explicit
        .map(PathBuf::from)
        .or_else(|| crate::envs::var_os("KELIR_CONFIG").map(PathBuf::from))
        .filter(|p| !p.as_os_str().is_empty());

    let mut merged = toml::Value::Table(Default::default());
    if let Some(path) = explicit {
        merge(&mut merged, read_toml(&path)?);
    } else {
        let machine = paths::machine_config_path();
        if machine.exists() {
            merge(&mut merged, read_toml(&machine)?);
        }
    }
    // The project layer always applies, even when a machine file was named
    // explicitly: a test or a user can point at one machine config without
    // turning off the project's own settings.
    let mut project_gates: Option<toml::Value> = None;
    if let Some(project) = find_project_config(cwd) {
        let mut layer = read_toml(&project)?;
        // The project cannot vouch for itself, and its gates need their own
        // trust decision: keep both out of the merged machine layer.
        if let Some(table) = layer.as_table_mut() {
            project_gates = table.remove("gates");
            if table.remove("trusted_projects").is_some() {
                eprintln!(
                    "kelir: warning: `trusted_projects` in a project config is ignored: it is a machine setting"
                );
            }
        }
        merge(&mut merged, layer);
    }

    if let Some(providers) = merged.get("providers").and_then(toml::Value::as_table)
        && let Some(other) = providers
            .keys()
            .find(|name| !PROVIDERS.contains(&name.as_str()))
    {
        return Err(unknown_provider_table(other));
    }
    let raw: RawConfig = merged
        .try_into()
        .map_err(|error| ConfigError(format!("invalid config.toml: {}", safe_de_error(&error))))?;
    let retry = retry_policy(&raw)?;
    let mcp = mcp_servers(&raw.mcp)?;

    let shell = resolve_shell(raw.shell);

    if let Some(other) = raw
        .providers
        .keys()
        .find(|name| !PROVIDERS.contains(&name.as_str()))
    {
        return Err(unknown_provider_table(other));
    }

    let mut providers = BTreeMap::new();
    for name in PROVIDERS {
        let entry = overlay(
            builtin_provider(name).unwrap_or_default(),
            raw.providers.get(name).cloned().unwrap_or_default(),
        );
        let kind = if name == PROVIDER_OPENROUTER {
            ProviderKind::OpenRouter
        } else {
            ProviderKind::OpencodeGo
        };
        let base_url = entry
            .base_url
            .filter(|url| !url.is_empty())
            .ok_or_else(|| ConfigError(format!("provider `{name}`: base_url is required")))?;
        let (effort, effort_explicit) = reasoning_effort(entry.reasoning_effort, name)?;
        providers.insert(
            name.to_string(),
            ProviderConfig {
                kind,
                base_url,
                // The key is resolved at session start ([`crate::provider::keys`]); load stays pure.
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

    let (system_prompt, prompt_path) = load_prompt(raw.prompt.as_deref())?;
    let compact_prompt = load_compact_prompt(raw.compaction.prompt.as_deref())?;
    let auto_compact_at = raw
        .compaction
        .auto_at
        .unwrap_or(crate::core::compact::AUTO_FRACTION);
    if !(0.0..=1.0).contains(&auto_compact_at) || auto_compact_at == 0.0 {
        return Err(ConfigError(format!(
            "compaction.auto_at must be between 0 and 1 (got {auto_compact_at})"
        )));
    }

    let gates = match &raw.gates {
        Some(value) => crate::features::gates::from_toml(value).map_err(ConfigError)?,
        None => Vec::new(),
    };
    let project_gates = match &project_gates {
        Some(value) => crate::features::gates::from_toml(value)
            .map_err(|error| ConfigError(format!("project config: {error}")))?
            .into_iter()
            .map(|mut def| {
                def.origin = "project config".into();
                def
            })
            .collect(),
        None => Vec::new(),
    };

    Ok(Config {
        compact_at_tokens: raw
            .compaction
            .at_tokens
            .unwrap_or(crate::core::compact::DEFAULT_SOFT_TARGET),
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
        compact_keep: raw
            .compaction
            .keep
            .unwrap_or(crate::core::compact::DEFAULT_KEEP),
        memory_dir: raw
            .memory_dir
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| expand_home(&path.to_string_lossy())),
        promote_after: promote_after(raw.promote_after)?,
        retry,
        warn_prompt_tokens: match raw.warn_prompt_tokens {
            Some(0) => None,
            Some(tokens) => Some(tokens),
            None => Some(crate::provider::usage::DEFAULT_WARN_PROMPT_TOKENS),
        },
        children: crate::features::children::Caps {
            depth: raw
                .children
                .max_depth
                .unwrap_or(crate::features::children::DEFAULT_MAX_DEPTH),
            per_agent: raw
                .children
                .max_per_agent
                .unwrap_or(crate::features::children::DEFAULT_MAX_CHILDREN),
            live: raw
                .children
                .max_live_descendants
                .unwrap_or(crate::features::children::DEFAULT_MAX_LIVE),
        },
        gates,
        project_gates,
        trusted_projects: raw
            .trusted_projects
            .iter()
            .map(|path| expand_home(&path.to_string_lossy()))
            .collect(),
        channel: crate::features::channel::Config {
            max_active: raw
                .channel
                .max_active
                .unwrap_or(crate::features::channel::DEFAULT_MAX_ACTIVE),
            per_conversation_rpm: raw
                .channel
                .per_conversation_rpm
                .unwrap_or(crate::features::channel::DEFAULT_PER_CONVERSATION_RPM),
            global_rpm: raw
                .channel
                .global_rpm
                .unwrap_or(crate::features::channel::DEFAULT_GLOBAL_RPM),
            fallback: raw
                .channel
                .fallback
                .unwrap_or_else(|| crate::features::channel::DEFAULT_FALLBACK.into()),
            spool_limit: raw
                .channel
                .spool_limit
                .unwrap_or(crate::features::channel::DEFAULT_SPOOL_LIMIT),
        },
        notify: notify(&raw.notify)?,
        mcp,
    })
}
