//! Model metadata from the provider, not from a table in this source.
//! Both providers publish it: OpenRouter's `GET {base}/models` carries `context_length` per model
//! and `supported_parameters` naming what it can do (`tools`, `parallel_tool_calls`,
//! `reasoning`...), and OpenCode Go's carries `limit.context` plus a `capabilities` object. kelir
//! reads whichever it is given, caches the normalised list beside the session state, and refreshes

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How long a fetched list is trusted before it is fetched again.
pub const CACHE_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// After a failed fetch, wait this long before trying again, so an offline box
/// pays the fetch timeout once an hour instead of on every start.
pub const RETRY_AFTER_MS: u64 = 60 * 60 * 1000;

/// How long the *background* fetch of `GET /models` is allowed to take.
pub const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a *cold* start waits for the list before its first request.
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
    /// Whether the model accepts images, when the provider states it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_calls: Option<bool>,
    /// Whether the model accepts the `reasoning_effort` field, when the provider lists
    /// `supported_parameters`: OpenRouter advertises it for the families that take it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cache {
    /// The provider name as configured (`openrouter`); informational, and part
    /// of [`endpoint_id`].
    pub provider: String,
    /// Which endpoint this answer belongs to: [`endpoint_id`] of the provider name and the base URL it was
    /// fetched from.
    #[serde(default)]
    pub endpoint: String,
    pub fetched_at_ms: u64,
    /// When the last fetch failed, so the next start does not retry at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_at_ms: Option<u64>,
    /// Why the last fetch did not land, in a few words (`http 401`, `offline`, `malformed`, `timeout`):
    /// what `state` and the TUI banner show instead of a silent `default`.
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

    /// A list fetched less than [`CACHE_TTL_MS`] ago.
    pub fn is_fresh(&self, now_ms: u64) -> bool {
        self.fetched_at_ms <= now_ms && now_ms - self.fetched_at_ms < CACHE_TTL_MS
    }

    /// A fetch failed less than [`RETRY_AFTER_MS`] ago, so asking again now would only pay the timeout once
    /// more.
    pub fn in_backoff(&self, now_ms: u64) -> bool {
        self.failed_at_ms
            .map(|at| at <= now_ms && now_ms - at < RETRY_AFTER_MS)
            .unwrap_or(false)
    }

    pub fn context_for(&self, model: &str) -> Option<u64> {
        lookup(&self.models, model).and_then(|info| info.context)
    }

    /// Whether the model takes images, when the provider says.
    pub fn vision_for(&self, model: &str) -> Option<bool> {
        lookup(&self.models, model).and_then(|info| info.vision)
    }
}

/// The entry for `model`: the exact id first, then case-insensitively, then the same last
/// segment **within the same vendor**.
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
    #[serde(default)]
    architecture: Option<RawArchitecture>,
}

/// OpenRouter's `architecture` object.
#[derive(Deserialize)]
struct RawArchitecture {
    #[serde(default)]
    input_modalities: Option<Vec<String>>,
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
    #[serde(default)]
    vision: Option<RawFlag>,
}

/// A capability gate that is either a flag or an object with `supported`.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawFlag {
    Flag(bool),
    Object(RawTools),
}

impl RawFlag {
    fn supported(&self) -> bool {
        match self {
            RawFlag::Flag(flag) => *flag,
            RawFlag::Object(object) => object.supported,
        }
    }
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
            // Three shapes say it, in this order: an explicit capability flag (the Go provider), the input
            // modalities (OpenRouter), and a capability named in `supported_parameters`.
            let modalities = model.architecture.as_ref().and_then(|architecture| {
                architecture.input_modalities.as_ref().map(|names| {
                    names.iter().any(|name| {
                        name.eq_ignore_ascii_case("image") || name.eq_ignore_ascii_case("images")
                    })
                })
            });
            let vision = model
                .capabilities
                .as_ref()
                .and_then(|capabilities| capabilities.vision.as_ref())
                .map(RawFlag::supported)
                .or(modalities)
                // A capability named in `supported_parameters` is a *yes* when it is there; its absence is not a no.
                .or_else(|| {
                    model.supported_parameters.as_ref().and_then(|names| {
                        names
                            .iter()
                            .any(|name| name == "vision" || name == "image" || name == "images")
                            .then_some(true)
                    })
                });
            let parallel_calls = model
                .supported_parameters
                .as_ref()
                .map(|names| names.iter().any(|name| name == "parallel_tool_calls"));
            // OpenRouter names the flat field `reasoning_effort` for the models that take it; some list
            // `reasoning` (the object form it is a shorthand for).
            let reasoning_effort = model.supported_parameters.as_ref().map(|names| {
                names
                    .iter()
                    .any(|name| name == "reasoning_effort" || name == "reasoning")
            });
            ModelInfo {
                id: model.id,
                vision,
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

/// Which endpoint a cached list belongs to: a slug of the provider name plus a hash of the
/// normalised base URL (`openrouter-a1b2c3d4e5f60718`).
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

/// The base URL as the cache key and the `/models` URL both see it: trimmed, no trailing `/`, no
/// trailing `/chat/completions`, lower-case scheme and host.
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

pub fn load(endpoint: &str) -> Option<Cache> {
    load_in(&crate::paths::state_dir(), endpoint)
}

pub fn load_in(dir: &Path, endpoint: &str) -> Option<Cache> {
    let text = std::fs::read_to_string(cache_path_in(dir, endpoint)).ok()?;
    let cache: Cache = serde_json::from_str(&text).ok()?;
    // A file that does not name this endpoint is another server's answer (a cache written before the
    // keying changed, a hand-copied state dir): ignoring it is what makes a cold start fall back to
    if cache.endpoint.is_empty() || cache.endpoint == endpoint {
        Some(cache)
    } else {
        None
    }
}

pub fn store(cache: &Cache) -> Result<(), String> {
    store_in(&crate::paths::state_dir(), cache)
}

/// Write the cache atomically: a temp file in the same directory, then a rename over the target.
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
        ulid::Ulid::generate()
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

/// Families whose models accept `parallel_tool_calls`, from what the providers document and what has
/// been measured.
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

pub fn parallel_calls(list: Option<&[ModelInfo]>, model: &str) -> bool {
    if let Some(info) = list.and_then(|models| lookup(models, model))
        && let Some(capable) = info.parallel_calls
    {
        return capable;
    }
    family_takes_parallel(model)
}

/// Whether `reasoning_effort` may be sent to this model.
pub fn reasoning_effort_allowed(list: Option<&[ModelInfo]>, model: &str, explicit: bool) -> bool {
    match list.and_then(|models| lookup(models, model)) {
        Some(info) => info.reasoning_effort.unwrap_or(true),
        None => explicit,
    }
}

/// The window to compact against, and where it came from, for the session log.
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
    (crate::core::compact::DEFAULT_WINDOW, "default")
}

/// A fetch failure in a few words, for `state` and the banner.
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

    /// OpenRouter: images are `architecture.input_modalities`, never a
    /// `supported_parameters` entry.
    const OPENROUTER_VISION: &str = r#"{"data":[
        {"id":"vision/model","context_length":200000,
         "supported_parameters":["tools"],
         "architecture":{"input_modalities":["text","image"],"output_modalities":["text"]}},
        {"id":"text/only","context_length":200000,
         "supported_parameters":["tools"],
         "architecture":{"input_modalities":["text"]}}
    ]}"#;

    /// the batching decision does not depend on a warm list. What the list says wins; a model
    /// it does not know falls back to its family; a model nobody knows is not asked.
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

    /// the effort gate, in the three cases the provider's list can be in.
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

    #[test]
    fn vision_is_read_from_whichever_field_the_provider_fills() {
        // OpenRouter states it as an input modality; the parameters list does
        // not mention images at all.
        let models = parse(OPENROUTER_VISION).unwrap();
        assert_eq!(models[0].vision, Some(true));
        assert_eq!(models[1].vision, Some(false));
        // The Go shape's capability object, as a flag and as `supported`.
        let go = r#"{"data":[
            {"id":"a/yes","capabilities":{"vision":{"supported":true}}},
            {"id":"b/no","capabilities":{"vision":false}},
            {"id":"c/silent","capabilities":{"tools":{"supported":true}}}
        ]}"#;
        let models = parse(go).unwrap();
        assert_eq!(models[0].vision, Some(true));
        assert_eq!(models[1].vision, Some(false));
        assert_eq!(models[2].vision, None, "silence is not a refusal");
        // And the cache answers per model.
        let cache = Cache::new("p", "http://example.invalid/v1", models, 0);
        assert_eq!(cache.vision_for("a/yes"), Some(true));
        assert_eq!(cache.vision_for("b/no"), Some(false));
        assert_eq!(cache.vision_for("c/silent"), None);
        assert_eq!(cache.vision_for("d/unknown"), None);
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

    /// the cache belongs to an endpoint. A list written for one base URL is not read for
    /// another, so a proxy or a gateway cannot inherit numbers from the server they replaced.
    #[test]
    fn the_cache_is_keyed_by_the_endpoint_not_the_provider_name() {
        let dir = std::env::temp_dir().join(format!("kelir-models-{}", ulid::Ulid::generate()));
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

    /// A config provider name never reaches a path as-is, and two
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

    /// the last-segment fallback matches a differently-prefixed id of the
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
            (crate::core::compact::DEFAULT_WINDOW, "default")
        );
        assert_eq!(
            resolve(None, None, false, "unknown/model"),
            (crate::core::compact::DEFAULT_WINDOW, "default")
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
