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

/// How long the first step waits for `GET /models`. The list is metadata, not
/// the work, so a slow answer must not hold a turn; it was 1.5 s, which a
/// 480-model list through a proxy can miss (STEER-6 review S2), so the wait is
/// longer and a timeout is no longer remembered as a failure.
pub const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(4_000);

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
    /// Whether the model accepts several tool calls in one assistant message.
    /// Advertised by OpenRouter in `supported_parameters`; Go states it
    /// nowhere, so `None` there means "kelir asks anyway" (see
    /// [`crate::provider::build_request`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_calls: Option<bool>,
    /// Whether the model accepts the `reasoning_effort` field, when the
    /// provider lists `supported_parameters`: OpenRouter advertises it for the
    /// families that take it. `None` means "the provider says nothing", which
    /// is not the same as "no": see the effort gate in `session.rs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<bool>,
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
            let parallel_calls = model
                .supported_parameters
                .as_ref()
                .map(|names| names.iter().any(|name| name == "parallel_tool_calls"));
            // OpenRouter names the flat field `reasoning_effort` for the models
            // that take it; some list `reasoning` (the object form it is a
            // shorthand for). Either name means the model takes reasoning
            // controls, which is what the gate needs to know.
            let reasoning_effort = model.supported_parameters.as_ref().map(|names| {
                names
                    .iter()
                    .any(|name| name == "reasoning_effort" || name == "reasoning")
            });
            ModelInfo {
                id: model.id,
                context: model
                    .context_length
                    .or(model.limit.and_then(|limit| limit.context))
                    .filter(|context| *context > 0),
                tools,
                parallel_calls,
                reasoning_effort,
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

/// Families whose models accept `parallel_tool_calls`, from what the providers
/// document and what has been measured. This is the *fallback* for a model the
/// provider's list does not know (a first session, a slow list, a proxy that
/// answers nothing): without it, whether the loop asks for batching would
/// depend on a cache that may be an hour stale or empty (STEER-6 review S2 —
/// live: a 480-model list took > 1.5 s through a forwarding proxy, the cache
/// was written empty, and the request had no `parallel_tool_calls` although the
/// model advertises it).
///
/// It is a *prefix/substring* test on the model id, deliberately conservative:
/// a family named here supports it for every model the providers serve, and a
/// model the provider's own list is authoritative for wins over this table.
pub const PARALLEL_CALL_FAMILIES: [&str; 12] = [
    "deepseek/",
    "deepseek-v",
    "anthropic/",
    "claude-",
    "openai/",
    "gpt-4",
    "gpt-5",
    "gpt-oss",
    "google/",
    "gemini-",
    "qwen/",
    "kimi",
];

/// Does a model id name a family known to take several tool calls in one
/// message? Only the fallback: see [`parallel_calls`].
pub fn family_takes_parallel(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    PARALLEL_CALL_FAMILIES
        .iter()
        .any(|family| model.starts_with(family) || model.contains(family))
}

/// Whether the loop may ask this model for several tool calls in one message.
///
/// Precedence: the provider's own list (`supported_parameters`), then the
/// built-in family table, then `false` — a model nobody says anything about is
/// not asked, because not every gateway accepts the field. `listed` is the
/// list as this process has it (cache or a fetch); `None`/no entry means the
/// provider said nothing about this model, not that it said "no".
pub fn parallel_calls(list: Option<&[ModelInfo]>, model: &str) -> bool {
    if let Some(info) = list.and_then(|models| lookup(models, model))
        && let Some(capable) = info.parallel_calls
    {
        return capable;
    }
    family_takes_parallel(model)
}

/// Whether `reasoning_effort` may be sent to this model.
///
/// `listed` is the provider's list (see [`parallel_calls`]); `explicit` says the
/// person configured an effort for this provider, which is the one case where an
/// unknown model is sent the field anyway (they asked for it).
///
/// * the list has the model with a `supported_parameters` list -> only when it
///   names `reasoning_effort` (or the `reasoning` object form it is shorthand
///   for): a provider that says a model does not take it is believed, and a
///   400 on every step is the thing this avoids;
/// * the list has the model but no capability list at all (OpenCode Go) ->
///   nothing is known, so kelir's default still goes; a gateway that refuses it
///   is caught by the one-shot 400 retry in `session.rs`;
/// * the list does not have the model (a first session, a proxy, a new model)
///   -> only when the person configured an effort, because an unknown gateway
///   is the case most likely to refuse an unknown field.
pub fn reasoning_effort_allowed(list: Option<&[ModelInfo]>, model: &str, explicit: bool) -> bool {
    match list.and_then(|models| lookup(models, model)) {
        Some(info) => info.reasoning_effort.unwrap_or(true),
        None => explicit,
    }
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
        // `supported_parameters` is read for both capabilities kelir acts on.
        assert_eq!(models[0].parallel_calls, Some(true));
        assert_eq!(models[0].reasoning_effort, Some(true));
        assert_eq!(models[1].parallel_calls, Some(false));
        assert_eq!(models[1].reasoning_effort, Some(false));
    }

    /// S2: the batching decision does not depend on a warm list. What the list
    /// says wins; a model it does not know falls back to its family; a model
    /// nobody knows is not asked.
    #[test]
    fn parallel_calls_falls_back_to_the_model_family() {
        let models = parse(OPENROUTER).unwrap();
        // The list is authoritative, both ways.
        assert!(parallel_calls(
            Some(&models),
            "deepseek/deepseek-v4.1-flash-1017"
        ));
        assert!(!parallel_calls(Some(&models), "some/no-tools"));
        // No list at all (a first session, a slow fetch): the family answers.
        assert!(parallel_calls(None, "deepseek/deepseek-v4.1-flash-1017"));
        assert!(parallel_calls(None, "anthropic/claude-sonnet-4.5"));
        assert!(parallel_calls(None, "deepseek-v4.1-flash"));
        assert!(!parallel_calls(None, "acme/mystery-1"));
        assert!(!parallel_calls(None, "test/model"));
        // An empty cache (a failed fetch) is the same as no list.
        let empty = parse(r#"{"data":[]}"#).unwrap();
        assert!(parallel_calls(
            Some(&empty),
            "deepseek/deepseek-v4.1-flash-1017"
        ));
        // A model outside the list but inside a known family.
        assert!(parallel_calls(Some(&models), "openai/gpt-5-mini"));
        assert!(!parallel_calls(Some(&models), "unknown/thing"));
    }

    /// S3: the effort gate, in the three cases the provider's list can be in.
    #[test]
    fn reasoning_effort_is_gated_on_what_the_provider_advertises() {
        let models = parse(OPENROUTER).unwrap();
        // Advertised -> allowed, default or explicit.
        assert!(reasoning_effort_allowed(
            Some(&models),
            "deepseek/deepseek-v4.1-flash-1017",
            false
        ));
        // Listed without it -> not allowed, even when configured: the provider
        // said no, and a 400 on every step is the thing to avoid.
        assert!(!reasoning_effort_allowed(
            Some(&models),
            "some/no-tools",
            true
        ));
        // Unknown model -> only when the person asked for it.
        assert!(!reasoning_effort_allowed(Some(&models), "acme/new", false));
        assert!(reasoning_effort_allowed(Some(&models), "acme/new", true));
        // No list at all -> only when explicitly configured.
        assert!(!reasoning_effort_allowed(None, "deepseek/x", false));
        assert!(reasoning_effort_allowed(None, "deepseek/x", true));
        // A provider that lists no capabilities at all (Go) says nothing: the
        // default stays, and a gateway that refuses it is caught by the retry.
        let go = parse(GO).unwrap();
        assert!(reasoning_effort_allowed(
            Some(&go),
            "deepseek-v4.1-flash",
            false
        ));
        assert!(reasoning_effort_allowed(
            Some(&go),
            "deepseek-v4.1-flash",
            true
        ));
    }

    /// The family table is a fallback, so it has to stay narrow: an id nobody
    /// recognises must not become "yes".
    #[test]
    fn the_family_table_is_conservative() {
        for model in [
            "deepseek/deepseek-v4.1-flash-1017",
            "anthropic/claude-sonnet-4.5",
            "openai/gpt-4o-mini",
            "google/gemini-2.5-flash",
            "qwen/qwen3-coder",
            "moonshotai/kimi-k2",
        ] {
            assert!(family_takes_parallel(model), "{model}");
        }
        for model in ["acme/mystery", "test/model", "some/no-tools", ""] {
            assert!(!family_takes_parallel(model), "{model}");
        }
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
