//! One OpenAI Chat Completions client for both providers.
//!
//! OpenRouter wants the session id in the body (`session_id`) for sticky
//! routing; OpenCode Go wants `x-opencode-session` and a `kelir/<version>`
//! User-Agent header. Neither ever gets `provider.order` (it would undo
//! stickiness). `stream_options.include_usage` is always on so cache hits are
//! measured rather than assumed.

use std::time::Duration;

use serde_json::{Value, json};

use crate::chat::ChatMessage;
use crate::config::{ProviderConfig, ProviderKind};
use crate::stream::{Assembler, Delta};
use crate::{VERSION, sse::SseParser};

/// A request ready to send: URL, headers and body.
#[derive(Debug, Clone)]
pub struct BuiltRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

impl BuiltRequest {
    /// The same request without one body field: what a retry sends when the
    /// provider answered 400 *because* of that field (STEER-6 review S3). The
    /// headers and everything else stay byte-identical, so the cache prefix
    /// does not move.
    pub fn without_field(&self, field: &str) -> BuiltRequest {
        let mut body = self.body.clone();
        if let Some(object) = body.as_object_mut() {
            object.remove(field);
        }
        BuiltRequest {
            url: self.url.clone(),
            headers: self.headers.clone(),
            body,
        }
    }

    pub fn has_field(&self, field: &str) -> bool {
        self.body.get(field).is_some()
    }
}

/// Does an error message name `field` as the thing it refused? Providers answer
/// an unknown or unsupported body field with a 400 whose `param`/`message` names
/// it (`"param": "reasoning_effort"` on OpenRouter, "unsupported parameter" in
/// the text elsewhere), so the name has to appear somewhere in the body.
pub fn names_field(message: &str, field: &str) -> bool {
    message.to_ascii_lowercase().contains(field)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderError {
    pub message: String,
    pub retryable: bool,
    /// The HTTP status, when the failure was a response rather than the
    /// transport (a socket that died has none).
    pub status: Option<u16>,
    /// What the response's own headers asked for (`Retry-After`, a reset
    /// header), when they said anything kelir could read.
    pub retry_after: Option<Duration>,
    /// The stream had already begun a tool call: that step may have work ahead
    /// of it, so it is not re-sent (an assistant message with a half-read call
    /// is not a request any provider would take twice).
    pub tool_call_started: bool,
}

impl ProviderError {
    pub fn new(message: impl Into<String>, retryable: bool) -> Self {
        Self {
            message: message.into(),
            retryable,
            status: None,
            retry_after: None,
            tool_call_started: false,
        }
    }

    /// A response kelir must not act on: the status decides whether another
    /// request is worth making.
    pub fn http(status: u16, message: impl Into<String>, retry_after: Option<Duration>) -> Self {
        Self {
            message: message.into(),
            retryable: crate::retry::status_is_retryable(status),
            status: Some(status),
            retry_after,
            tool_call_started: false,
        }
    }

    /// A failure of the request itself (DNS, TLS, connect, a socket that died
    /// mid-stream). Always worth another request — unless a tool call of the
    /// step had already begun.
    pub fn transport(message: impl Into<String>) -> Self {
        Self::new(message, true)
    }

    /// The short reason for a wait's visible line.
    pub fn reason(&self) -> String {
        match self.status {
            Some(code) => format!("{} ({code})", crate::retry::status_reason(code)),
            None => self.message.clone(),
        }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ProviderError {}

/// How many times a *resumed turn* is tried again after a retryable failure:
/// surviving an outage is what `resume` is for. A step's own retries are bounded
/// by [`crate::retry::Budget`], which is a waiting budget rather than a count.
pub const TURN_ATTEMPTS: u32 = 3;

/// Backoff before attempt `attempt + 1`: 200 ms, 400 ms, 800 ms…
///
/// Kept for the parts that still want a short, fixed schedule (a resumed
/// *turn* is tried again while this lasts). A step's own retries use
/// [`crate::retry`], which honours the provider's hints and fills the waiting
/// budget.
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(200u64.saturating_mul(1u64 << attempt.min(5)))
}

/// Models that need an explicit, forward-moving cache breakpoint on
/// OpenRouter (DeepSeek, OpenAI, Grok, Moonshot, Z.AI and Gemini 2.5+ cache
/// automatically).
pub fn needs_explicit_cache_control(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.contains("anthropic/") || model.contains("claude") || model.contains("qwen")
}

pub fn chat_completions_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else {
        format!("{base}/chat/completions")
    }
}

/// Build the request for one step. `messages` already starts with the frozen
/// system message.
///
/// `parallel_calls` says whether the model advertises `parallel_tool_calls`.
/// When it does, the field is set: the loop already runs the calls of one
/// assistant message concurrently and appends the results in call order, and
/// nothing else told the model so — 47 % of M5-A's steps were a single
/// read-only call, and batching them 3-wide would have saved 5M prompt tokens.
pub fn build_request(
    cfg: &ProviderConfig,
    session_id: &str,
    model: &str,
    messages: &[ChatMessage],
    tools: &[Value],
    cache_ttl: Option<&str>,
    parallel_calls: bool,
) -> BuiltRequest {
    let mut headers = vec![
        ("content-type".to_string(), "application/json".to_string()),
        ("accept".to_string(), "text/event-stream".to_string()),
        ("user-agent".to_string(), format!("kelir/{VERSION}")),
    ];
    if let Some(key) = cfg.key.as_deref().filter(|key| !key.is_empty()) {
        headers.push(("authorization".to_string(), format!("Bearer {key}")));
    }
    if cfg.kind == ProviderKind::OpencodeGo {
        headers.push(("x-opencode-session".to_string(), session_id.to_string()));
    } else if let Some(header) = &cfg.session_header {
        headers.push((header.to_ascii_lowercase(), session_id.to_string()));
    }
    for (name, value) in &cfg.extra_headers {
        headers.push((name.to_ascii_lowercase(), value.clone()));
    }

    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.to_vec());
    }
    if parallel_calls && cfg.kind == ProviderKind::OpenRouter {
        body["parallel_tool_calls"] = Value::Bool(true);
    }
    // Hidden reasoning is asked for explicitly. Dogfood runs on the provider's
    // default effort re-derived the whole design every step (M5-A steps 38-62:
    // 49 % of all completion tokens in 29 % of the wall clock); `medium` is the
    // loop's default and the knob is per provider, because only the gateway
    // knows what it honours.
    if let Some(effort) = cfg.reasoning_effort.as_deref().filter(|e| !e.is_empty()) {
        body["reasoning_effort"] = Value::String(effort.to_string());
    }
    if cfg.kind == ProviderKind::OpenRouter {
        body["session_id"] = Value::String(session_id.to_string());
        if needs_explicit_cache_control(model) {
            let mut cache = json!({"type": "ephemeral"});
            if let Some(ttl) = cache_ttl {
                cache["ttl"] = Value::String(ttl.to_string());
            }
            body["cache_control"] = cache;
        }
    }

    BuiltRequest {
        url: chat_completions_url(&cfg.base_url),
        headers,
        body,
    }
}

#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    cfg: ProviderConfig,
    session_id: String,
    cache_ttl: Option<String>,
}

impl Client {
    pub fn new(
        cfg: ProviderConfig,
        session_id: &str,
        cache_ttl: Option<String>,
    ) -> Result<Self, ProviderError> {
        if cfg.key.is_none() && cfg.key_env.is_some() {
            return Err(ProviderError::new(
                format!(
                    "missing API key: set {}",
                    cfg.key_env.as_deref().unwrap_or("<key env>")
                ),
                false,
            ));
        }
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .build()
            .map_err(|error| ProviderError::new(format!("http client: {error}"), false))?;
        Ok(Self {
            http,
            cfg,
            session_id: session_id.to_string(),
            cache_ttl,
        })
    }

    pub fn provider_name(&self) -> ProviderKind {
        self.cfg.kind
    }

    /// What the `error` event and the screen say about a refused request.
    ///
    /// An outage (a retryable status) gets the status and its class and nothing
    /// else: it is a wait, and no part of the body helps a person or a script
    /// decide anything. A refusal (400/401/403/404/422) keeps its (bounded,
    /// secret-redacted) body, because "unknown model" is the answer that lets a
    /// human fix the invocation.
    fn message_for(&self, code: u16, body: &str) -> String {
        let reason = crate::retry::status_reason(code);
        if crate::retry::status_is_retryable(code) {
            return format!("http {code}: {reason}");
        }
        let body = crate::util::redact(body.trim(), self.cfg.key.as_deref());
        let body = crate::util::truncate_chars(&body, 500);
        if body.is_empty() {
            format!("http {code}: {reason}")
        } else {
            format!("http {code}: {reason}: {body}")
        }
    }

    /// The provider's own model list, for the metadata kelir does not hardcode
    /// (context window, tool support). Both providers serve `GET /models`;
    /// OpenRouter's is public, Go's wants the same key as a completion.
    pub async fn models(&self) -> Result<Vec<crate::models::ModelInfo>, String> {
        let url = format!("{}/models", self.cfg.base_url.trim_end_matches('/'));
        let mut request = self
            .http
            .get(&url)
            .timeout(Duration::from_secs(5))
            .header("accept", "application/json");
        if let Some(key) = self.cfg.key.as_deref() {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("models: {error}"))?;
        if !response.status().is_success() {
            return Err(format!("models: HTTP {}", response.status()));
        }
        let body = response
            .text()
            .await
            .map_err(|error| format!("models: {error}"))?;
        crate::models::parse(&body)
    }

    pub fn build(&self, model: &str, messages: &[ChatMessage], tools: &[Value]) -> BuiltRequest {
        build_request(
            &self.cfg,
            &self.session_id,
            model,
            messages,
            tools,
            self.cache_ttl.as_deref(),
            false,
        )
    }

    /// The same, with the model's own `parallel_tool_calls` capability, which
    /// the session resolves from the provider's model list.
    pub fn build_with(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: &[Value],
        parallel_calls: bool,
    ) -> BuiltRequest {
        build_request(
            &self.cfg,
            &self.session_id,
            model,
            messages,
            tools,
            self.cache_ttl.as_deref(),
            parallel_calls,
        )
    }

    /// Send one request and assemble the stream. Any connect, transport or
    /// truncated-stream failure comes back as a (usually retryable) error.
    pub async fn stream(&self, request: &BuiltRequest) -> Result<Assembler, ProviderError> {
        self.stream_with(request, &mut |_| {}).await
    }

    /// The same, handing each new piece of text to `on_delta` as it arrives.
    /// The callback must return quickly: it runs on the stream's own task.
    ///
    /// A failure carries what the retry policy needs: the HTTP status (when
    /// there was a response), what the response's headers asked for, and
    /// whether a tool call of that step had already begun — a step whose tool
    /// call is in flight is not re-sent, because it may already have had an
    /// effect.
    pub async fn stream_with(
        &self,
        request: &BuiltRequest,
        on_delta: &mut dyn FnMut(Delta),
    ) -> Result<Assembler, ProviderError> {
        let mut builder = self.http.post(&request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let response = builder
            .json(&request.body)
            .send()
            .await
            .map_err(|error| ProviderError::transport(format!("request failed: {error}")))?;

        let status = response.status();
        if !status.is_success() {
            let code = status.as_u16();
            // Read the headers before the body: they are gone with it, and they
            // are what says how long the provider wants kelir to wait.
            let retry_after = crate::retry::header_hint(
                response
                    .headers()
                    .iter()
                    .filter_map(|(name, value)| Some((name.as_str(), value.to_str().ok()?))),
                std::time::SystemTime::now(),
            );
            // The body is read to drain the connection, and it is *not* put on
            // the event: a gateway that quotes the request (or a 4xx that
            // prints the key) would write a credential into `events.jsonl` and
            // into NDJSON stdout (STEER-5 review S3).
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::http(
                code,
                self.message_for(code, &body),
                retry_after,
            ));
        }

        let mut response = response;
        let mut parser = SseParser::new();
        let mut assembler = Assembler::new(self.cfg.reasoning_field.clone());

        loop {
            if assembler.done {
                break;
            }
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    parser.push(&chunk);
                    while let Some(event) = parser.next_event() {
                        assembler
                            .feed_event(&event)
                            .map_err(|message| partial_error(message, &assembler))?;
                        for delta in assembler.take_pending() {
                            on_delta(delta);
                        }
                        if assembler.done {
                            break;
                        }
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    return Err(partial_error(
                        format!("stream interrupted: {error}"),
                        &assembler,
                    ));
                }
            }
        }

        if !assembler.done && assembler.finish_reason.is_none() {
            return Err(partial_error(
                "stream ended before the model finished".to_string(),
                &assembler,
            ));
        }
        Ok(assembler)
    }
}

/// A failure part-way through a response. Retryable — the request can simply
/// be re-sent — unless a tool call of the step had already been streamed, in
/// which case the step may have work ahead of it and is not re-run (the tool
/// layer settles the call as `unknown`).
fn partial_error(message: impl Into<String>, assembler: &Assembler) -> ProviderError {
    let tool_call_started = !assembler.tool_calls().is_empty();
    let mut error = ProviderError::transport(message);
    error.tool_call_started = tool_call_started;
    if tool_call_started {
        error.retryable = false;
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::ToolCall;
    use crate::config::ProviderKind;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn openrouter() -> ProviderConfig {
        ProviderConfig {
            kind: ProviderKind::OpenRouter,
            base_url: "https://openrouter.ai/api/v1".into(),
            key_env: Some("OPENROUTER_API_KEY".into()),
            key: Some("sk-test".into()),
            extra_headers: BTreeMap::new(),
            reasoning_field: None,
            reasoning_effort: None,
            reasoning_effort_explicit: false,
            session_header: None,
            max_concurrent_requests: None,
        }
    }

    fn go() -> ProviderConfig {
        ProviderConfig {
            kind: ProviderKind::OpencodeGo,
            base_url: "https://opencode.ai/inference/go/v1".into(),
            key_env: Some("KELIR_GO_API_KEY".into()),
            key: Some("oc_sk-test".into()),
            extra_headers: BTreeMap::new(),
            reasoning_field: None,
            reasoning_effort: None,
            reasoning_effort_explicit: false,
            session_header: Some("x-opencode-session".into()),
            max_concurrent_requests: None,
        }
    }

    fn header<'a>(request: &'a BuiltRequest, name: &str) -> Option<&'a str> {
        request
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn conversation() -> Vec<ChatMessage> {
        vec![ChatMessage::system("sys"), ChatMessage::user("task")]
    }

    #[test]
    fn openrouter_sends_session_id_in_the_body() {
        let request = build_request(
            &openrouter(),
            "ses_01ABC",
            "deepseek/deepseek-chat",
            &conversation(),
            &[],
            None,
            false,
        );
        assert_eq!(request.body["session_id"], "ses_01ABC");
        assert_eq!(request.body["stream"], true);
        assert_eq!(request.body["stream_options"]["include_usage"], true);
        assert!(header(&request, "x-opencode-session").is_none());
        assert!(request.body.get("provider").is_none());
        assert_eq!(request.url, "https://openrouter.ai/api/v1/chat/completions");
    }

    #[test]
    fn go_sends_the_session_header_and_ua_and_no_body_session() {
        let request = build_request(
            &go(),
            "ses_01ABC",
            "glm-5",
            &conversation(),
            &[],
            None,
            false,
        );
        assert_eq!(header(&request, "x-opencode-session"), Some("ses_01ABC"));
        assert_eq!(
            header(&request, "user-agent"),
            Some(concat!("kelir/", env!("CARGO_PKG_VERSION")))
        );
        assert_eq!(header(&request, "authorization"), Some("Bearer oc_sk-test"));
        assert!(request.body.get("session_id").is_none());
        assert!(request.body.get("cache_control").is_none());
        assert_eq!(
            request.url,
            "https://opencode.ai/inference/go/v1/chat/completions"
        );
    }

    #[test]
    fn cache_control_only_for_anthropic_and_qwen() {
        for model in [
            "anthropic/claude-sonnet-4.5",
            "qwen/qwen3-coder",
            "some/claude-x",
        ] {
            let request = build_request(
                &openrouter(),
                "ses_1",
                model,
                &conversation(),
                &[],
                None,
                false,
            );
            assert_eq!(
                request.body["cache_control"],
                json!({"type": "ephemeral"}),
                "{model}"
            );
        }
        for model in ["deepseek/deepseek-chat", "openai/gpt-5", "x-ai/grok-4"] {
            let request = build_request(
                &openrouter(),
                "ses_1",
                model,
                &conversation(),
                &[],
                None,
                false,
            );
            assert!(
                request.body.get("cache_control").is_none(),
                "{model} must rely on automatic caching"
            );
        }
    }

    #[test]
    fn cache_ttl_is_configurable() {
        let request = build_request(
            &openrouter(),
            "ses_1",
            "anthropic/claude-sonnet-4.5",
            &conversation(),
            &[],
            Some("1h"),
            false,
        );
        assert_eq!(
            request.body["cache_control"],
            json!({"type": "ephemeral", "ttl": "1h"})
        );
    }

    #[test]
    fn go_does_not_get_cache_control_even_for_claude() {
        let request = build_request(
            &go(),
            "ses_1",
            "anthropic/claude-sonnet-4.5",
            &conversation(),
            &[],
            Some("1h"),
            false,
        );
        assert!(request.body.get("cache_control").is_none());
    }

    #[test]
    fn extra_headers_and_tools_are_included() {
        let mut cfg = go();
        cfg.extra_headers.insert("x-title".into(), "kelir".into());
        let tools = vec![json!({"type": "function"})];
        let request = build_request(&cfg, "ses_1", "m", &conversation(), &tools, None, false);
        assert_eq!(header(&request, "x-title"), Some("kelir"));
        assert_eq!(request.body["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn messages_are_sent_verbatim() {
        let messages = conversation();
        let request = build_request(&openrouter(), "s", "m", &messages, &[], None, false);
        let sent = request.body["messages"].as_array().unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0]["role"], "system");
        assert_eq!(sent[0]["content"], "sys");
        assert_eq!(sent[1]["content"], "task");
    }

    #[test]
    fn prefix_only_grows_across_steps() {
        let tools = vec![json!({"type": "function"})];
        let mut messages = vec![ChatMessage::system("sys"), ChatMessage::user("task")];

        let step_one = build_request(&openrouter(), "s", "m", &messages, &tools, None, false);
        let first_prefix = step_one.body["messages"].as_array().unwrap().clone();

        let call = ToolCall {
            id: "call_1".into(),
            name: "bash".into(),
            args: json!({"command": "ls"}),
        };
        messages.push(ChatMessage::assistant("", std::slice::from_ref(&call)));
        messages.push(ChatMessage::tool("call_1", "a.txt"));
        messages.push(ChatMessage::assistant("done", &[]));

        let step_two = build_request(&openrouter(), "s", "m", &messages, &tools, None, false);
        let second = step_two.body["messages"].as_array().unwrap().clone();

        assert_eq!(second.len(), first_prefix.len() + 3);
        assert_eq!(&second[..first_prefix.len()], first_prefix.as_slice());
        // Tools are frozen too: byte-identical from step to step.
        assert_eq!(step_one.body["tools"], step_two.body["tools"]);
    }

    #[test]
    fn base_url_with_full_path_is_left_alone() {
        assert_eq!(
            chat_completions_url("http://127.0.0.1:8080/v1/chat/completions"),
            "http://127.0.0.1:8080/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("http://127.0.0.1:8080/v1/"),
            "http://127.0.0.1:8080/v1/chat/completions"
        );
    }

    #[test]
    fn a_refusal_keeps_a_redacted_body_and_an_outage_keeps_none() {
        let mut cfg = openrouter();
        cfg.key = Some("fixture-key-not-a-credential".into());
        let client = Client::new(cfg, "ses_1", None).unwrap();
        // An outage: the status and its class, never the body.
        assert_eq!(
            client.message_for(
                503,
                "upstream said: Bearer fixture-key-not-a-credential refused"
            ),
            "http 503: provider error"
        );
        assert_eq!(
            client.message_for(429, "slow down"),
            "http 429: rate limited"
        );
        // A refusal: the body is the answer, minus any credential.
        assert_eq!(
            client.message_for(400, "unknown model; key fixture-key-not-a-credential"),
            "http 400: provider refused: unknown model; key [redacted]"
        );
        assert_eq!(client.message_for(404, "  "), "http 404: provider refused");
    }

    #[test]
    fn missing_key_is_a_clear_config_error() {
        let mut cfg = openrouter();
        cfg.key = None;
        let error = Client::new(cfg, "ses_1", None).unwrap_err();
        assert!(error.message.contains("OPENROUTER_API_KEY"));
        assert!(!error.retryable);
    }

    #[test]
    fn backoff_grows_then_caps() {
        assert_eq!(backoff(0), Duration::from_millis(200));
        assert_eq!(backoff(1), Duration::from_millis(400));
        assert_eq!(backoff(2), Duration::from_millis(800));
        assert_eq!(backoff(9), Duration::from_millis(6_400));
    }

    #[test]
    fn reasoning_effort_is_sent_only_when_it_is_set() {
        let mut cfg = go();
        cfg.reasoning_effort = None;
        let request = build_request(&cfg, "s", "m", &conversation(), &[], None, false);
        assert!(request.body.get("reasoning_effort").is_none());

        cfg.reasoning_effort = Some("low".into());
        let request = build_request(&cfg, "s", "m", &conversation(), &[], None, false);
        assert_eq!(request.body["reasoning_effort"], json!("low"));
    }

    #[test]
    fn parallel_tool_calls_is_asked_for_only_when_the_model_takes_it() {
        let tools = vec![json!({"type": "function"})];
        let off = build_request(
            &openrouter(),
            "s",
            "m",
            &conversation(),
            &tools,
            None,
            false,
        );
        assert!(off.body.get("parallel_tool_calls").is_none());
        let on = build_request(&openrouter(), "s", "m", &conversation(), &tools, None, true);
        assert_eq!(on.body["parallel_tool_calls"], json!(true));
        // Go does not document the field, so it is not guessed at.
        let go_on = build_request(&go(), "s", "m", &conversation(), &tools, None, true);
        assert!(go_on.body.get("parallel_tool_calls").is_none());
    }
}
