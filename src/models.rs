//! Model metadata from the provider, not from a table in this source.
//!
//! Both providers publish it: OpenRouter's `GET {base}/models` carries
//! `context_length` per model and `supported_parameters` naming what it can do
//! (`tools`, `parallel_tool_calls`, `reasoning`, ...), and OpenCode Go's carries
//! `limit.context` plus a `capabilities` object. kelir reads whichever it is
//! given, caches the normalised list beside the session state, and refreshes it
//! once a day.
//!
//! A configuration override (`[model_windows]`, then `window`) still wins: it is
//! the one thing the provider cannot know. With neither a fetch nor a cache the
//! window is [`crate::compact::DEFAULT_WINDOW`], and `session_started` says so.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How long a fetched list is trusted before it is fetched again.
pub const CACHE_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// After a failed fetch, wait this long before trying again, so an offline box
/// pays the fetch timeout once an hour instead of on every start.
pub const RETRY_AFTER_MS: u64 = 60 * 60 * 1000;

/// One model, with only the fields kelir acts on: a provider adding a field
/// cannot break a session, and a field kelir does not need is not stored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    /// Context window in tokens, when the provider states one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
    /// Whether the model accepts tool calls, when the provider states it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
}

/// The cached answer for one provider.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cache {
    pub provider: String,
    pub fetched_at_ms: u64,
    /// When the last fetch failed, so the next start does not retry at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_at_ms: Option<u64>,
    #[serde(default)]
    pub models: Vec<ModelInfo>,
}

impl Cache {
    pub fn new(provider: &str, models: Vec<ModelInfo>, now_ms: u64) -> Self {
        Self {
            provider: provider.to_string(),
            fetched_at_ms: now_ms,
            failed_at_ms: None,
            models,
        }
    }

    pub fn is_fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.fetched_at_ms) < CACHE_TTL_MS
    }

    pub fn in_backoff(&self, now_ms: u64) -> bool {
        self.failed_at_ms
            .map(|at| now_ms.saturating_sub(at) < RETRY_AFTER_MS)
            .unwrap_or(false)
    }

    pub fn context_for(&self, model: &str) -> Option<u64> {
        lookup(&self.models, model).and_then(|info| info.context)
    }
}

/// The entry for `model`: the exact id first, then case-insensitively, then the
/// same last segment (a Go id `deepseek-v4.1-flash` and an OpenRouter id
/// `deepseek/deepseek-v4.1-flash-1017` are different models and do not match).
pub fn lookup<'a>(models: &'a [ModelInfo], model: &str) -> Option<&'a ModelInfo> {
    models
        .iter()
        .find(|info| info.id == model)
        .or_else(|| {
            models
                .iter()
                .find(|info| info.id.eq_ignore_ascii_case(model))
        })
        .or_else(|| {
            let last = model.rsplit('/').next().unwrap_or(model);
            models
                .iter()
                .find(|info| info.id.rsplit('/').next() == Some(last))
        })
}

#[derive(Deserialize)]
struct RawResponse {
    #[serde(default)]
    data: Vec<RawModel>,
}

#[derive(Deserialize)]
struct RawModel {
    #[serde(default)]
    id: String,
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    limit: Option<RawLimit>,
    #[serde(default)]
    supported_parameters: Option<Vec<String>>,
    #[serde(default)]
    capabilities: Option<RawCapabilities>,
}

#[derive(Deserialize)]
struct RawLimit {
    #[serde(default)]
    context: Option<u64>,
}

#[derive(Deserialize)]
struct RawCapabilities {
    #[serde(default)]
    tools: Option<RawTools>,
}

#[derive(Deserialize)]
struct RawTools {
    #[serde(default)]
    supported: bool,
}

/// Parse a `GET /models` body from either provider into the normalised list.
pub fn parse(body: &str) -> Result<Vec<ModelInfo>, String> {
    let raw: RawResponse = serde_json::from_str(body)
        .map_err(|error| format!("models: cannot read the list: {error}"))?;
    Ok(raw
        .data
        .into_iter()
        .filter(|model| !model.id.is_empty())
        .map(|model| {
            let tools = model
                .capabilities
                .as_ref()
                .and_then(|capabilities| capabilities.tools.as_ref())
                .map(|tools| tools.supported)
                .or_else(|| {
                    model
                        .supported_parameters
                        .as_ref()
                        .map(|names| names.iter().any(|name| name == "tools"))
                });
            ModelInfo {
                id: model.id,
                context: model
                    .context_length
                    .or(model.limit.and_then(|limit| limit.context))
                    .filter(|context| *context > 0),
                tools,
            }
        })
        .collect())
}

/// Where the provider's list is cached: `<state>/models-<provider>.json`.
pub fn cache_path_in(dir: &Path, provider: &str) -> PathBuf {
    dir.join(format!("models-{provider}.json"))
}

pub fn cache_path(provider: &str) -> PathBuf {
    cache_path_in(&crate::paths::state_dir(), provider)
}

pub fn load(provider: &str) -> Option<Cache> {
    load_in(&crate::paths::state_dir(), provider)
}

pub fn load_in(dir: &Path, provider: &str) -> Option<Cache> {
    let text = std::fs::read_to_string(cache_path_in(dir, provider)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn store(cache: &Cache) -> Result<(), String> {
    store_in(&crate::paths::state_dir(), cache)
}

pub fn store_in(dir: &Path, cache: &Cache) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("models: cannot create {}: {error}", dir.display()))?;
    let path = cache_path_in(dir, &cache.provider);
    let text = serde_json::to_string(cache).map_err(|error| format!("models: {error}"))?;
    std::fs::write(&path, text)
        .map_err(|error| format!("models: cannot write {}: {error}", path.display()))
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// The window to compact against, and where it came from, for the session log.
///
/// `over` is a configuration override for this model; `fetched_now` says the
/// list came from the provider in this process rather than from its cache.
pub fn resolve(
    over: Option<u64>,
    cache: Option<&Cache>,
    fetched_now: bool,
    model: &str,
) -> (u64, &'static str) {
    if let Some(window) = over.filter(|window| *window > 0) {
        return (window, "config");
    }
    if let Some(window) = cache.and_then(|cache| cache.context_for(model)) {
        return (window, if fetched_now { "api" } else { "cache" });
    }
    (crate::compact::DEFAULT_WINDOW, "default")
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPENROUTER: &str = r#"{"data":[
        {"id":"deepseek/deepseek-v4.1-flash-1017","context_length":1048576,
         "supported_parameters":["tools","parallel_tool_calls","reasoning"],
         "pricing":{"prompt":"0.0000003"}},
        {"id":"some/no-tools","context_length":8192,"supported_parameters":["temperature"]}
    ]}"#;

    const GO: &str = r#"{"object":"list","data":[
        {"id":"deepseek-v4.1-flash","limit":{"context":1000000,"output":384000},
         "capabilities":{"tools":{"supported":true}}}
    ]}"#;

    #[test]
    fn the_openrouter_shape_is_read() {
        let models = parse(OPENROUTER).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].context, Some(1_048_576));
        assert_eq!(models[0].tools, Some(true));
        assert_eq!(models[1].tools, Some(false));
    }

    #[test]
    fn the_go_shape_is_read() {
        let models = parse(GO).unwrap();
        assert_eq!(models[0].id, "deepseek-v4.1-flash");
        assert_eq!(models[0].context, Some(1_000_000));
        assert_eq!(models[0].tools, Some(true));
    }

    #[test]
    fn a_body_without_data_or_broken_json_is_an_error() {
        assert!(parse("not json").is_err());
        assert!(parse("{}").unwrap().is_empty());
        assert!(
            parse(r#"{"data":[{"context_length":10}]}"#)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn lookup_prefers_the_exact_id_and_never_guesses_a_variant() {
        let models = parse(OPENROUTER).unwrap();
        assert_eq!(
            lookup(&models, "deepseek/deepseek-v4.1-flash-1017").map(|m| m.id.as_str()),
            Some("deepseek/deepseek-v4.1-flash-1017")
        );
        assert!(lookup(&models, "deepseek/deepseek-v4.1-flash").is_none());
        assert_eq!(
            lookup(&models, "SOME/NO-TOOLS").map(|m| m.id.as_str()),
            Some("some/no-tools")
        );
    }

    #[test]
    fn the_cache_round_trips_and_knows_its_age() {
        let dir = std::env::temp_dir().join(format!("kelir-models-{}", ulid::Ulid::new()));
        let models = parse(GO).unwrap();
        let cache = Cache::new("opencode-go", models, 1_000);
        store_in(&dir, &cache).unwrap();
        let read = load_in(&dir, "opencode-go").unwrap();
        assert_eq!(read.models, cache.models);
        assert!(read.is_fresh(1_000 + CACHE_TTL_MS - 1));
        assert!(!read.is_fresh(1_000 + CACHE_TTL_MS));
        assert!(!read.in_backoff(1_000 + RETRY_AFTER_MS));
        let mut failed = read.clone();
        failed.failed_at_ms = Some(5_000);
        assert!(failed.in_backoff(5_000 + RETRY_AFTER_MS - 1));
        assert!(!failed.in_backoff(5_000 + RETRY_AFTER_MS));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_config_wins_over_the_provider_and_the_provider_over_the_default() {
        let models = parse(OPENROUTER).unwrap();
        let cache = Cache::new("openrouter", models, 0);
        assert_eq!(
            resolve(
                Some(32_000),
                Some(&cache),
                true,
                "deepseek/deepseek-v4.1-flash-1017"
            ),
            (32_000, "config")
        );
        assert_eq!(
            resolve(
                None,
                Some(&cache),
                true,
                "deepseek/deepseek-v4.1-flash-1017"
            ),
            (1_048_576, "api")
        );
        assert_eq!(
            resolve(
                None,
                Some(&cache),
                false,
                "deepseek/deepseek-v4.1-flash-1017"
            ),
            (1_048_576, "cache")
        );
        assert_eq!(
            resolve(None, Some(&cache), true, "unknown/model"),
            (crate::compact::DEFAULT_WINDOW, "default")
        );
        assert_eq!(
            resolve(None, None, false, "unknown/model"),
            (crate::compact::DEFAULT_WINDOW, "default")
        );
        // A zero is not a window; the next source answers.
        assert_eq!(
            resolve(
                Some(0),
                Some(&cache),
                true,
                "deepseek/deepseek-v4.1-flash-1017"
            ),
            (1_048_576, "api")
        );
    }
}
