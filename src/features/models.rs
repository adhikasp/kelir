//! The models/window feature: provider metadata, the context window and model swaps.
//! All model state lives here; the core never names a field of it.

use tokio::sync::oneshot;

use crate::core::boundary::{Cx, Feature};
use crate::provider::Client;
use crate::tui::note_event;

/// The provider's model list and how this process came by it.
pub struct ModelList {
    pub cache: Option<crate::models::Cache>,
    pub fetched: bool,
    pub why: Option<String>,
}

impl ModelList {
    pub fn from_cache(cache: Option<crate::models::Cache>) -> Self {
        Self {
            cache,
            fetched: false,
            why: None,
        }
    }

    fn with_why(mut self, why: Option<String>) -> Self {
        self.why = why;
        self
    }
}

/// A `GET /models` running beside the session, never on its critical path.
pub struct Fetch {
    rx: Option<oneshot::Receiver<ModelList>>,
}

impl Fetch {
    pub fn none() -> Self {
        Self { rx: None }
    }

    pub fn pending(rx: oneshot::Receiver<ModelList>) -> Self {
        Self { rx: Some(rx) }
    }

    pub fn is_running(&self) -> bool {
        self.rx.is_some()
    }

    /// The answer, if the fetch has already finished. Never waits: a fetch that
    /// is still running costs this call nothing at all.
    pub fn take_ready(&mut self) -> Option<ModelList> {
        let rx = self.rx.as_mut()?;
        match rx.try_recv() {
            Ok(list) => {
                self.rx = None;
                Some(list)
            }
            Err(oneshot::error::TryRecvError::Empty) => None,
            Err(oneshot::error::TryRecvError::Closed) => {
                // The task ended without sending (it panicked): nothing to
                // apply, and the cache on disk is what the session keeps using.
                self.rx = None;
                Some(ModelList::from_cache(None))
            }
        }
    }

    /// Wait at most `budget` for the answer. Running out of budget leaves the
    /// fetch running for the next boundary.
    pub async fn wait(&mut self, budget: std::time::Duration) -> Option<ModelList> {
        let rx = self.rx.as_mut()?;
        match tokio::time::timeout(budget, rx).await {
            Ok(Ok(list)) => {
                self.rx = None;
                Some(list)
            }
            Ok(Err(_)) => {
                self.rx = None;
                Some(ModelList::from_cache(None))
            }
            // Not ready: keep the receiver, and the fetch keeps running.
            Err(_) => None,
        }
    }
}

/// Spawn the fetch, unless the cache on disk already answers.
pub fn start_fetch(client: &Client, provider: &str, endpoint: &str) -> Fetch {
    let now = crate::models::now_ms();
    let cached = crate::models::load(endpoint);
    if let Some(cache) = &cached
        && (cache.is_fresh(now) || cache.in_backoff(now))
    {
        return Fetch::none();
    }
    let client = client.clone();
    let provider = provider.to_string();
    let endpoint = endpoint.to_string();
    let (tx, rx) = oneshot::channel();
    // The task holds only the sender, so dropping the session (or the grace)
    // never cancels it mid-write; a receiver that is gone just loses the send.
    tokio::spawn(async move {
        let list = fetch_models(&client, &provider, &endpoint, cached).await;
        let _ = tx.send(list);
    });
    Fetch::pending(rx)
}

/// One fetch of `GET /models`, its result written beside the session state.
async fn fetch_models(
    client: &Client,
    provider: &str,
    endpoint: &str,
    cached: Option<crate::models::Cache>,
) -> ModelList {
    let now = crate::models::now_ms();
    let base_url = client.base_url();
    match tokio::time::timeout(crate::models::FETCH_TIMEOUT, client.models()).await {
        Ok(Ok(models)) => {
            let cache = crate::models::Cache::new(provider, &base_url, models, now);
            let _ = crate::models::store(&cache);
            ModelList {
                cache: Some(cache),
                fetched: true,
                why: None,
            }
        }
        Ok(Err(reason)) => {
            let mut cache = cached
                .unwrap_or_else(|| crate::models::Cache::new(provider, &base_url, Vec::new(), 0));
            cache.endpoint = endpoint.to_string();
            cache.failed_at_ms = Some(now);
            cache.error = Some(reason.clone());
            let _ = crate::models::store(&cache);
            ModelList {
                cache: Some(cache),
                fetched: false,
                why: Some(reason),
            }
        }
        // Slow, not wrong: the cached list keeps its age and the next start
        // asks again. Nothing is remembered as a failure.
        Err(_) => ModelList::from_cache(cached).with_why(Some("timeout".to_string())),
    }
}

/// What a start uses before the fetch lands: the cached list (stale numbers
/// beat the default) and the reason `state` gives for it.
fn cached_list(provider: &str, base_url: &str) -> ModelList {
    let endpoint = crate::models::endpoint_id(provider, base_url);
    let flushed = crate::models::load(&endpoint);
    let why = flushed
        .as_ref()
        .and_then(|cache| cache.error.clone())
        .or_else(|| flushed.is_none().then(|| "fetching".to_string()));
    ModelList::from_cache(flushed).with_why(why)
}

/// The list to start with, and the fetch still running for the rest.
pub async fn begin_models(client: &Client, provider: &str, base_url: &str) -> (ModelList, Fetch) {
    let endpoint = crate::models::endpoint_id(provider, base_url);
    let mut fetch = start_fetch(client, provider, &endpoint);
    if !fetch.is_running() {
        // Fresh (or a failure still inside its backoff): read from disk.
        let list = ModelList::from_cache(crate::models::load(&endpoint));
        let why = list.cache.as_ref().and_then(|cache| cache.error.clone());
        return (list.with_why(why), fetch);
    }
    let early = cached_list(provider, base_url);
    match fetch.wait(crate::models::START_GRACE).await {
        Some(list) => (list, fetch),
        // Not a failure, a slow endpoint: use what is cached and let the fetch
        // finish beside the session.
        None => (early, fetch),
    }
}

/// Everything this feature owns: the cache, the in-flight fetch, the window and why it is what it is.
pub struct Models {
    pub window: u64,
    pub window_source: &'static str,
    pub window_note: Option<String>,
    pub window_model: Option<String>,
    pub models: Option<crate::models::Cache>,
    pub fetch: Option<Fetch>,
}

impl Models {
    pub fn new(
        window: u64,
        window_source: &'static str,
        window_note: Option<String>,
        models: Option<crate::models::Cache>,
        fetch: Option<Fetch>,
        model: &str,
    ) -> Self {
        Self {
            window,
            window_source,
            window_note,
            // Seeded from the startup answer, so a later `/model` swap announces a changed window.
            window_model: Some(model.to_string()),
            models,
            fetch,
        }
    }

    /// The banner's one line about the window, when the window is *not* the provider's.
    pub fn banner_line(&self) -> Option<String> {
        if self.window_source == "api" || self.window_source == "config" {
            return None;
        }
        // "fetching" is not a degradation, it is "the answer is not in yet": `apply_list` says the
        // real reason when it lands, so the banner does not name a window it is about to correct.
        if self.window_note.as_deref() == Some("the model list: fetching") {
            return None;
        }
        match &self.window_note {
            Some(note) => Some(format!(
                "window {} tokens ({}) · {note}",
                self.window, self.window_source
            )),
            // A stale cache or a configured number needs no apology; a fallback to
            // the default with nothing to say about why does.
            None if self.window_source == "default" => Some(format!(
                "window {} tokens (default) · the model list did not answer",
                self.window
            )),
            None => None,
        }
    }

    /// Take the metadata fetch's answer if it has landed, and apply it: the list a `model` swap
    /// or a later step will read, and the window if the configuration does not override it.
    pub async fn apply_models(&mut self, cx: &mut Cx<'_>) {
        let ready = match self.fetch.as_mut() {
            Some(fetch) => fetch.take_ready(),
            None => None,
        };
        if let Some(list) = ready {
            self.apply_list(list, cx);
        }
    }

    /// Apply one answer from the metadata fetch: the list every later step and a `model` swap
    /// read, and the window when the configuration does not override it.
    pub fn apply_list(&mut self, list: ModelList, cx: &mut Cx<'_>) {
        if let Some(cache) = list.cache {
            self.models = Some(cache);
        }
        let over = cx.cfg.override_window(cx.model);
        let (window, source) =
            crate::models::resolve(over, self.models.as_ref(), list.fetched, cx.model);
        // The startup answer is silent: the banner no longer carries the window. Two cases
        // are not silent, and both are one dim line:
        let note = window_note(over, list.why.as_deref(), source);
        let previous = self.window_model.replace(cx.model.clone());
        let swapped = previous.is_some_and(|model| model != *cx.model);
        let moved = swapped && (window != self.window || source != self.window_source);
        // A start whose fetch had not landed says the fallback here instead: the banner could only say
        // "fetching" (the answer did not exist yet), and the reason is what a person needs (F-window).
        let fell_back = source == "default"
            && note.is_some()
            && (note != self.window_note || source != self.window_source);
        if moved || fell_back {
            let mut line = format!("» window {window} tokens ({source})");
            if let Some(note) = &note
                && fell_back
            {
                line.push_str(" · ");
                line.push_str(note);
            }
            cx.emit(crate::tui::View::Log(note_event(&line)));
        }
        self.window = window;
        self.window_source = source;
        self.window_note = note;
    }

    /// `model` swaps the provider/model for the rest of the session.
    pub fn swap_model(&mut self, target: &str, cx: &mut Cx<'_>) -> Result<(), String> {
        let (provider, model) = split_model_target(target, &cx.cfg.providers);
        let provider = provider
            .map(str::to_string)
            .unwrap_or_else(|| cx.provider_name.clone());
        let model = model.to_string();
        let provider_cfg = if provider == *cx.provider_name {
            // The same provider: the key already in memory is the live one.
            cx.active_provider.clone()
        } else {
            // A swap to another provider needs *that* provider's key, resolved the same way the start
            // resolved this one.
            let mut provider_cfg = cx
                .cfg
                .provider(&provider)
                .map_err(|error| error.to_string())?;
            crate::credentials::resolve(&mut provider_cfg)?;
            provider_cfg
        };
        // The governor belongs to the provider a request goes to.
        *cx.governor = governor_for(&provider_cfg);
        let client = Client::new(provider_cfg.clone(), cx.id, cx.cfg.cache_ttl.clone())
            .map_err(|error| error.message)?;
        *cx.provider_name = provider.clone();
        *cx.model = model.clone();
        *cx.client = client;
        *cx.active_provider = provider_cfg;
        // Another model, possibly another provider: resolve from that endpoint's cache (never the network mid-
        // turn) and start a fetch beside the session when it is stale.
        let base_url = cx.client.base_url();
        let endpoint = crate::models::endpoint_id(&provider, &base_url);
        self.models = crate::models::load(&endpoint);
        self.fetch = Some(start_fetch(cx.client, &provider, &endpoint));
        let over = cx.cfg.override_window(&model);
        let (window, source) = crate::models::resolve(over, self.models.as_ref(), false, &model);
        // A swap is the one moment a person must be told the window changed.
        if window != self.window || source != self.window_source {
            cx.emit(crate::tui::View::Log(note_event(format!(
                "» window {window} tokens ({source})"
            ))));
        }
        self.window = window;
        self.window_source = source;
        let why = self.models.as_ref().and_then(|cache| cache.error.clone());
        self.window_note = window_note(over, why.as_deref(), source);
        Ok(())
    }
}

/// Why this start has the window it has, or `None` when the answer needs no explanation.
pub fn window_note(over: Option<u64>, why: Option<&str>, source: &str) -> Option<String> {
    let why = why?;
    if over.filter(|window| *window > 0).is_some() || source == "api" || source == "cache" {
        return None;
    }
    Some(format!("the model list: {why}"))
}

impl Feature for Models {
    fn name(&self) -> &'static str {
        "models"
    }

    async fn before_step(&mut self, cx: &mut Cx<'_>) {
        self.apply_models(cx).await;
    }

    fn status(&self, out: &mut serde_json::Map<String, serde_json::Value>) {
        out.insert("window".into(), serde_json::json!(self.window));
        out.insert(
            "window_source".into(),
            serde_json::json!(self.window_source),
        );
        out.insert("window_note".into(), serde_json::json!(self.window_note));
        out.insert(
            "models_fetching".into(),
            serde_json::json!(self.fetch.as_ref().is_some_and(Fetch::is_running)),
        );
    }
}

/// Split `provider/model` when the prefix names a configured provider.
fn split_model_target<'a>(
    target: &'a str,
    providers: &'a std::collections::BTreeMap<String, crate::config::ProviderConfig>,
) -> (Option<&'a str>, &'a str) {
    match target.split_once('/') {
        Some((prefix, rest)) if providers.contains_key(prefix) && !rest.is_empty() => {
            (Some(prefix), rest)
        }
        _ => (None, target),
    }
}

/// The opt-in governor for one provider config.
fn governor_for(provider: &crate::config::ProviderConfig) -> Option<crate::governor::Governor> {
    provider.max_concurrent_requests.map(|slots| {
        crate::governor::Governor::new(&crate::session::provider_base(provider), slots)
    })
}

#[cfg(test)]
pub fn test_split_model_target<'a>(
    target: &'a str,
    providers: &'a std::collections::BTreeMap<String, crate::config::ProviderConfig>,
) -> (Option<&'a str>, &'a str) {
    split_model_target(target, providers)
}
