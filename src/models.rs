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
//! window is [`crate::compact::DEFAULT_WINDOW`], and the reason a start landed
//! there is recorded (see [`Cache::error`]).
//!
//! The cache belongs to an *endpoint*, not to a provider name: the file is named
//! after [`endpoint_id`] (a slug of the provider name plus a hash of the
//! normalised base URL), so a proxy or a second gateway that shares a state dir
//! never inherits another server's numbers (STEER-1 review S1/S5/N5). It is
//! written temp-file-then-rename, so a reader never sees a half-written list
//! (S3).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How long a fetched list is trusted before it is fetched again.
pub const CACHE_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// After a failed fetch, wait this long before trying again, so an offline box
/// pays the fetch timeout once an hour instead of on every start.
pub const RETRY_AFTER_MS: u64 = 60 * 60 * 1000;

/// How long the *background* fetch of `GET /models` is allowed to take. It is
/// not a bound on a turn: the fetch runs beside the session, so a 1 MB list on
/// a slow link is no longer paid for by the first request (STEER-1 review S2),
/// and a timeout is not remembered as a failure (the next start tries again).
pub const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a *cold* start waits for the list before its first request. The
/// capabilities the list answers (`reasoning_effort`, `parallel_tool_calls`)
/// are part of the first request, so the start gives a local list (or one
/// through a proxy) this long to arrive; a slower answer keeps being fetched in
/// the background and is applied when it lands. The *window* is only needed at
/// the first `before_step` compaction check, which is after the first step, so
/// this grace bounds a capability decision and never a turn.
pub const START_GRACE: std::time::Duration = std::time::Duration::from_millis(250);

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

/// The cached answer for one endpoint.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cache {
    /// The provider name as configured (`openrouter`); informational, and part
    /// of [`endpoint_id`].
    pub provider: String,
    /// Which endpoint this answer belongs to: [`endpoint_id`] of the provider
    /// name and the base URL it was fetched from. A cache that does not match
    /// is ignored, so a proxy or a second gateway never inherits the first
    /// one's numbers (STEER-1 review S1).
    #[serde(default)]
    pub endpoint: String,
    pub fetched_at_ms: u64,
    /// When the last fetch failed, so the next start does not retry at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_at_ms: Option<u64>,
    /// Why the last fetch did not land, in a few words (`http 401`, `offline`,
    /// `malformed`, `timeout`): what `state` and the TUI banner show instead of
    /// a silent `default` (STEER-1 review N4). Never a key, a header or a body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub models: Vec<ModelInfo>,
}

impl Cache {
    pub fn new(provider: &str, base_url: &str, models: Vec<ModelInfo>, now_ms: u64) -> Self {
        Self {
            provider: provider.to_string(),
            endpoint: endpoint_id(provider, base_url),
            fetched_at_ms: now_ms,
            failed_at_ms: None,
            error: None,
            models,
        }
    }

    /// A list fetched less than [`CACHE_TTL_MS`] ago. A stamp in the future (a
    /// clock that was ahead when the file was written, a VM restore) is not
    /// fresh: refetching is cheap and wrong numbers are not (N2).
    pub fn is_fresh(&self, now_ms: u64) -> bool {
        self.fetched_at_ms <= now_ms && now_ms - self.fetched_at_ms < CACHE_TTL_MS
    }

    /// A fetch failed less than [`RETRY_AFTER_MS`] ago, so asking again now
    /// would only pay the timeout once more. A failure stamped in the future is
    /// clock skew, not an hour of backoff: the `at <= now` guard keeps it from
    /// locking the refresh out for as long as the skew (STEER-1 review N2).
    pub fn in_backoff(&self, now_ms: u64) -> bool {
        self.failed_at_ms
            .map(|at| at <= now_ms && now_ms - at < RETRY_AFTER_MS)
            .unwrap_or(false)
    }

    pub fn context_for(&self, model: &str) -> Option<u64> {
        lookup(&self.models, model).and_then(|info| info.context)
    }
}

/// The entry for `model`: the exact id first, then case-insensitively, then the
/// same last segment **within the same vendor**.
///
/// The last step exists because a Go id (`deepseek-v4.1-flash`) and an
/// OpenRouter id (`deepseek/deepseek-v4.1-flash-1017`) name the same family with
/// different prefixes, and a list served by one gateway may use either shape.
/// It never crosses vendors: `deepseek/x` does not match `fireworks/x`, even
/// though that entry has the same last segment, because the two ids name
/// different models that happen to share a name (STEER-1 review N3). A query
/// with no `/` at all (the Go shape) matches either shape, which is what it is
/// for.
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
            let vendor = model.rfind('/').map(|at| &model[..at]);
            let last = model.rsplit('/').next().unwrap_or(model);
            models.iter().find(|info| {
                info.id.rsplit('/').next() == Some(last)
                    && match vendor {
                        // A qualified query only matches an entry of the same
                        // vendor (or an unqualified one, the Go shape).
                        Some(vendor) => !info.id.contains('/') || info.id.starts_with(vendor),
                        // An unqualified query (the Go shape) matches either.
                        None => true,
                    }
            })
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

/// Which endpoint a cached list belongs to: a slug of the provider name plus a
/// hash of the normalised base URL (`openrouter-a1b2c3d4e5f60718`).
///
/// The hash is what makes two endpoints distinct (a proxy, a gateway, a
/// `KELIR_BASE_URL` stand-in), and the slug is what keeps the file name
/// readable and *safe*: a config provider key is never interpolated into a
/// path as-is, so a name like `..\x` cannot write outside the state dir
/// (STEER-1 review S1/S5/N5). The normalisation is the one
/// [`crate::provider::models_url`] uses, so two spellings of the same endpoint
/// (`http://h/v1` and `http://h/v1/`) share one cache.
pub fn endpoint_id(provider: &str, base_url: &str) -> String {
    format!(
        "{}-{}",
        slug(provider),
        crate::util::fnv1a64_hex(normalise_base(base_url).as_bytes())
    )
}

/// The provider name as a file-name-safe slug: ASCII alphanumerics kept, every
/// other run collapsed to one `-`, at most 24 characters. Never empty.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= 24 {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "provider".to_string()
    } else {
        trimmed
    }
}

/// The base URL as the cache key and the `/models` URL both see it: trimmed,
/// no trailing `/`, no trailing `/chat/completions`, lower-case scheme and
/// host. Two spellings of one endpoint must not make two caches.
pub fn normalise_base(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    let base = base.strip_suffix("/chat/completions").unwrap_or(base);
    let base = base.trim_end_matches('/');
    match base.split_once("://") {
        Some((scheme, rest)) => {
            let (host, path) = match rest.find('/') {
                Some(at) => rest.split_at(at),
                None => (rest, ""),
            };
            format!(
                "{}://{}{}",
                scheme.to_ascii_lowercase(),
                host.to_ascii_lowercase(),
                path
            )
        }
        None => base.to_string(),
    }
}

/// Where one endpoint's list is cached: `<state>/models-<endpoint id>.json`.
pub fn cache_path_in(dir: &Path, endpoint: &str) -> PathBuf {
    dir.join(format!("models-{endpoint}.json"))
}

pub fn cache_path(endpoint: &str) -> PathBuf {
    cache_path_in(&crate::paths::state_dir(), endpoint)
}

pub fn load(endpoint: &str) -> Option<Cache> {
    load_in(&crate::paths::state_dir(), endpoint)
}

pub fn load_in(dir: &Path, endpoint: &str) -> Option<Cache> {
    let text = std::fs::read_to_string(cache_path_in(dir, endpoint)).ok()?;
    let cache: Cache = serde_json::from_str(&text).ok()?;
    // A file that does not name this endpoint is another server's answer (a
    // cache written before the keying changed, a hand-copied state dir):
    // ignoring it is what makes a cold start fall back to `default` instead of
    // reusing numbers that belong to a different gateway (STEER-1 review S1).
    if cache.endpoint.is_empty() || cache.endpoint == endpoint {
        Some(cache)
    } else {
        None
    }
}

pub fn store(cache: &Cache) -> Result<(), String> {
    store_in(&crate::paths::state_dir(), cache)
}

/// Write the cache atomically: a temp file in the same directory, then a
/// rename over the target. A reader sees the old list or the new one, never a
/// truncated file, and a process killed mid-write leaves the old list intact
/// (STEER-1 review S3). No fsync: the file is a cache, and a lost write only
/// costs one refetch.
pub fn store_in(dir: &Path, cache: &Cache) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("models: cannot create {}: {error}", dir.display()))?;
    let path = cache_path_in(dir, &cache.endpoint);
    let text = serde_json::to_string(cache).map_err(|error| format!("models: {error}"))?;
    // One temp name per write: two processes writing at once must not share it
    // (the rename is atomic, so the last one wins and neither tears the file).
    let temp = dir.join(format!(
        "models-{}.{}.tmp",
        cache.endpoint,
        ulid::Ulid::new()
    ));
    std::fs::write(&temp, text)
        .map_err(|error| format!("models: cannot write {}: {error}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        format!("models: cannot replace {}: {error}", path.display())
    })
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

/// A fetch failure in a few words, for `state` and the banner (STEER-1 review
/// N4). The provider's own message is never carried: it can hold a URL, a body
/// fragment or a header value, and none of it belongs in a status line.
pub fn short_reason(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") {
        return "timeout".to_string();
    }
    if lower.contains("refused")
        || lower.contains("dns")
        || lower.contains("connect")
        || lower.contains("network")
        || lower.contains("unreachable")
        || lower.contains("builder error")
        || lower.contains("error sending request")
    {
        return "offline".to_string();
    }
    if lower.contains("cannot read the list") || lower.contains("malformed") {
        return "malformed".to_string();
    }
    if let Some(rest) = error.strip_prefix("http ")
        && rest.chars().take(3).all(|ch| ch.is_ascii_digit())
    {
        return format!("http {}", &rest[..3]);
    }
    "failed".to_string()
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
        let base = "http://127.0.0.1:1234/v1";
        let endpoint = endpoint_id("opencode-go", base);
        let cache = Cache::new("opencode-go", base, models, 1_000);
        store_in(&dir, &cache).unwrap();
        let read = load_in(&dir, &endpoint).unwrap();
        assert_eq!(read.models, cache.models);
        assert_eq!(read.endpoint, endpoint, "the endpoint is recorded");
        assert!(read.is_fresh(1_000 + CACHE_TTL_MS - 1));
        assert!(!read.is_fresh(1_000 + CACHE_TTL_MS));
        assert!(!read.in_backoff(1_000 + RETRY_AFTER_MS));
        let mut failed = read.clone();
        failed.failed_at_ms = Some(5_000);
        assert!(failed.in_backoff(5_000 + RETRY_AFTER_MS - 1));
        assert!(!failed.in_backoff(5_000 + RETRY_AFTER_MS));
        // N2: a stamp in the future is clock skew, not an hour of backoff, and
        // a list stamped in the future is not fresh either.
        let mut skewed = read.clone();
        skewed.failed_at_ms = Some(10_000_000);
        assert!(!skewed.in_backoff(5_000), "a future failure is not backoff");
        assert!(skewed.in_backoff(10_000_000 + RETRY_AFTER_MS - 1));
        let mut ahead = read.clone();
        ahead.fetched_at_ms = 10_000_000;
        assert!(!ahead.is_fresh(5_000), "a future fetch is not fresh");
        assert!(ahead.is_fresh(10_000_000));
        // The write leaves no temp file behind.
        let temps: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(temps.is_empty(), "a temp file was left: {temps:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// S1: the cache belongs to an endpoint. A list written for one base URL is
    /// not read for another, so a proxy or a gateway cannot inherit numbers
    /// from the server they replaced.
    #[test]
    fn the_cache_is_keyed_by_the_endpoint_not_the_provider_name() {
        let dir = std::env::temp_dir().join(format!("kelir-models-{}", ulid::Ulid::new()));
        let models = parse(OPENROUTER).unwrap();
        let gateway = endpoint_id("openrouter", "http://gateway.local/v1");
        let direct = endpoint_id("openrouter", "https://openrouter.ai/api/v1");
        assert_ne!(gateway, direct);
        store_in(
            &dir,
            &Cache::new(
                "openrouter",
                "http://gateway.local/v1",
                models.clone(),
                1_000,
            ),
        )
        .unwrap();
        assert!(load_in(&dir, &gateway).is_some());
        assert!(
            load_in(&dir, &direct).is_none(),
            "another endpoint must not read this cache"
        );
        // A cache file that names a different endpoint (written before the
        // keying changed, or copied) is ignored, not trusted.
        let path = cache_path_in(&dir, &direct);
        let mut wrong = Cache::new("openrouter", "http://gateway.local/v1", models, 1_000);
        wrong.endpoint = gateway.clone();
        std::fs::write(&path, serde_json::to_string(&wrong).unwrap()).unwrap();
        assert!(load_in(&dir, &direct).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// S5/N5: a config provider name never reaches a path as-is, and two
    /// spellings of one base URL share one cache.
    #[test]
    fn a_provider_name_is_a_slug_and_a_base_url_is_normalised() {
        for (name, expected) in [
            ("openrouter", "openrouter"),
            ("opencode-go", "opencode-go"),
            ("..\\x", "x"),
            ("../etc/passwd", "etc-passwd"),
            ("", "provider"),
        ] {
            assert_eq!(slug(name), expected, "{name}");
        }
        // No separator, no drive letter and no dot-dot survives.
        for name in ["..\\x", "../x", "C:\\Windows", "a/b"] {
            let id = endpoint_id(name, "http://h/v1");
            assert!(
                !id.contains('/') && !id.contains('\\') && !id.contains(".."),
                "{name} -> {id}"
            );
        }
        assert_eq!(
            endpoint_id("openrouter", "http://h/v1"),
            endpoint_id("openrouter", " http://H/v1/ "),
            "case and a trailing slash are not a different endpoint"
        );
        assert_eq!(
            endpoint_id("openrouter", "http://h/v1/chat/completions"),
            endpoint_id("openrouter", "http://h/v1"),
            "a full-path base URL is the same endpoint as its root"
        );
        assert_eq!(normalise_base("http://H/v1/"), "http://h/v1");
        // The scheme and the host fold case; the path does not (a gateway's path
        // can be case-sensitive).
        assert_eq!(
            normalise_base("HTTP://H/V1/chat/completions"),
            "http://h/V1"
        );
    }

    /// S3: a write replaces the file atomically, so a reader never sees a
    /// truncated list.
    #[test]
    fn the_cache_is_written_atomically() {
        let dir = std::env::temp_dir().join(format!("kelir-models-{}", ulid::Ulid::new()));
        let endpoint = endpoint_id("openrouter", "http://h/v1");
        let older = Cache::new(
            "openrouter",
            "http://h/v1",
            parse(OPENROUTER).unwrap(),
            1_000,
        );
        store_in(&dir, &older).unwrap();
        // A concurrent reader sees the old list or the new one, both complete.
        let path = cache_path_in(&dir, &endpoint);
        let mut seen = Vec::new();
        for index in 0..20 {
            let newer = Cache::new(
                "openrouter",
                "http://h/v1",
                parse(GO).unwrap(),
                1_000 + index,
            );
            store_in(&dir, &newer).unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            let cache: Cache =
                serde_json::from_str(&text).expect("a reader never sees a partial file");
            seen.push(cache.fetched_at_ms);
        }
        assert_eq!(seen.last().copied(), Some(1_019));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// N3: the last-segment fallback matches a differently-prefixed id of the
    /// same vendor, never another vendor's entry with the same last segment.
    #[test]
    fn the_last_segment_fallback_does_not_cross_vendors() {
        let models = parse(
            r#"{"data":[
                {"id":"fireworks/deepseek-v4.1-flash-1017","context_length":8000},
                {"id":"deepseek-v4.1-flash","context_length":1048576}
            ]}"#,
        )
        .unwrap();
        // Same vendor, different prefix shape: the Go id answers.
        assert_eq!(
            lookup(&models, "deepseek/deepseek-v4.1-flash").map(|m| m.id.as_str()),
            Some("deepseek-v4.1-flash")
        );
        // Another vendor with the same last segment must not answer.
        assert_eq!(
            lookup(&models, "acme/deepseek-v4.1-flash-1017").map(|m| m.id.as_str()),
            None
        );
        // An unqualified query (the Go shape) may match either.
        assert_eq!(
            lookup(&models, "deepseek-v4.1-flash-1017").map(|m| m.id.as_str()),
            Some("fireworks/deepseek-v4.1-flash-1017")
        );
        // The exact id always wins, whatever the vendors look like.
        assert_eq!(
            lookup(&models, "fireworks/deepseek-v4.1-flash-1017").map(|m| m.id.as_str()),
            Some("fireworks/deepseek-v4.1-flash-1017")
        );
    }

    /// N1: `/models` is derived from the same base as the completion URL, so a
    /// `base_url` that already names a path does not get a second one.
    #[test]
    fn the_models_url_is_derived_from_any_base() {
        assert_eq!(
            crate::provider::models_url("http://127.0.0.1:8080/v1"),
            "http://127.0.0.1:8080/v1/models"
        );
        assert_eq!(
            crate::provider::models_url("http://127.0.0.1:8080/v1/"),
            "http://127.0.0.1:8080/v1/models"
        );
        assert_eq!(
            crate::provider::models_url("http://127.0.0.1:8080/v1/chat/completions"),
            "http://127.0.0.1:8080/v1/models",
            "a full-path base URL must not become /v1/chat/completions/models"
        );
    }

    /// N4: a failure becomes a short reason for `state` and the banner, never
    /// the provider's own text.
    #[test]
    fn a_fetch_failure_gets_a_short_reason() {
        assert_eq!(short_reason("http 401"), "http 401");
        assert_eq!(short_reason("http 500"), "http 500");
        assert_eq!(
            short_reason(
                "error sending request for url (http://secret.example/v1/models): connection refused"
            ),
            "offline"
        );
        assert_eq!(short_reason("operation timed out"), "timeout");
        assert_eq!(
            short_reason("models: cannot read the list: expected value at line 1"),
            "malformed"
        );
        assert_eq!(short_reason("something else entirely"), "failed");
    }

    #[test]
    fn the_config_wins_over_the_provider_and_the_provider_over_the_default() {
        let models = parse(OPENROUTER).unwrap();
        let cache = Cache::new("openrouter", "http://h/v1", models, 0);
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
