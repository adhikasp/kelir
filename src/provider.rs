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

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderError {
    pub message: String,
    pub retryable: bool,
}

impl ProviderError {
    pub fn new(message: impl Into<String>, retryable: bool) -> Self {
        Self {
            message: message.into(),
            retryable,
        }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ProviderError {}

pub const MAX_ATTEMPTS: u32 = 3;

/// Backoff before attempt `attempt + 1`: 200 ms, 400 ms, 800 ms…
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
pub fn build_request(
    cfg: &ProviderConfig,
    session_id: &str,
    model: &str,
    messages: &[ChatMessage],
    tools: &[Value],
    cache_ttl: Option<&str>,
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

    pub fn build(&self, model: &str, messages: &[ChatMessage], tools: &[Value]) -> BuiltRequest {
        build_request(
            &self.cfg,
            &self.session_id,
            model,
            messages,
            tools,
            self.cache_ttl.as_deref(),
        )
    }

    /// Send one request and assemble the stream. Any connect, transport or
    /// truncated-stream failure comes back as a (usually retryable) error.
    pub async fn stream(&self, request: &BuiltRequest) -> Result<Assembler, ProviderError> {
        self.stream_with(request, &mut |_| {}).await
    }

    /// The same, handing each new piece of text to `on_delta` as it arrives.
    /// The callback must return quickly: it runs on the stream's own task.
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
            .map_err(|error| ProviderError::new(format!("request failed: {error}"), true))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let code = status.as_u16();
            return Err(ProviderError::new(
                format!(
                    "http {code}: {}",
                    crate::util::truncate_chars(body.trim(), 500)
                ),
                code >= 500 || code == 408 || code == 429,
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
                            .map_err(|message| ProviderError::new(message, true))?;
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
                    return Err(ProviderError::new(
                        format!("stream interrupted: {error}"),
                        true,
                    ));
                }
            }
        }

        if !assembler.done && assembler.finish_reason.is_none() {
            return Err(ProviderError::new(
                "stream ended before the model finished",
                true,
            ));
        }
        Ok(assembler)
    }
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
            session_header: None,
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
            session_header: Some("x-opencode-session".into()),
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
        let request = build_request(&go(), "ses_01ABC", "glm-5", &conversation(), &[], None);
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
            let request = build_request(&openrouter(), "ses_1", model, &conversation(), &[], None);
            assert_eq!(
                request.body["cache_control"],
                json!({"type": "ephemeral"}),
                "{model}"
            );
        }
        for model in ["deepseek/deepseek-chat", "openai/gpt-5", "x-ai/grok-4"] {
            let request = build_request(&openrouter(), "ses_1", model, &conversation(), &[], None);
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
        );
        assert!(request.body.get("cache_control").is_none());
    }

    #[test]
    fn extra_headers_and_tools_are_included() {
        let mut cfg = go();
        cfg.extra_headers.insert("x-title".into(), "kelir".into());
        let tools = vec![json!({"type": "function"})];
        let request = build_request(&cfg, "ses_1", "m", &conversation(), &tools, None);
        assert_eq!(header(&request, "x-title"), Some("kelir"));
        assert_eq!(request.body["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn messages_are_sent_verbatim() {
        let messages = conversation();
        let request = build_request(&openrouter(), "s", "m", &messages, &[], None);
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

        let step_one = build_request(&openrouter(), "s", "m", &messages, &tools, None);
        let first_prefix = step_one.body["messages"].as_array().unwrap().clone();

        let call = ToolCall {
            id: "call_1".into(),
            name: "bash".into(),
            args: json!({"command": "ls"}),
        };
        messages.push(ChatMessage::assistant("", std::slice::from_ref(&call)));
        messages.push(ChatMessage::tool("call_1", "a.txt"));
        messages.push(ChatMessage::assistant("done", &[]));

        let step_two = build_request(&openrouter(), "s", "m", &messages, &tools, None);
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
}
