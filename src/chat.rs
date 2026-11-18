//! Chat messages in the OpenAI Chat Completions shape.
//!
//! The fold produces these; the request builder serialises them unchanged, so
//! a request's message prefix is byte-identical from one step to the next.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::event::LoggedToolCall;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ApiFunction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ApiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(text: &str) -> Self {
        Self {
            role: "system".into(),
            content: Some(text.to_string()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(text: &str) -> Self {
        Self {
            role: "user".into(),
            content: Some(text.to_string()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// An assistant turn. A turn that only calls tools has `content: null`,
    /// which OpenAI-compatible APIs accept and expect.
    pub fn assistant(text: &str, calls: &[LoggedToolCall]) -> Self {
        let content = if text.is_empty() && !calls.is_empty() {
            None
        } else {
            Some(text.to_string())
        };
        let tool_calls = if calls.is_empty() {
            None
        } else {
            Some(calls.iter().map(ApiToolCall::from).collect())
        };
        Self {
            role: "assistant".into(),
            content,
            tool_calls,
            tool_call_id: None,
        }
    }

    pub fn tool(call_id: &str, content: &str) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: Some(call_id.to_string()),
        }
    }
}

impl From<&LoggedToolCall> for ApiToolCall {
    fn from(call: &LoggedToolCall) -> Self {
        ApiToolCall {
            id: call.id.clone(),
            kind: "function".into(),
            function: ApiFunction {
                name: call.name.clone(),
                arguments: arguments_string(&call.args),
            },
        }
    }
}

/// Tool-argument text for the wire: objects are serialised, a string stored
/// as-is (some providers stream arguments as pre-encoded JSON text).
pub fn arguments_string(args: &Value) -> String {
    match args {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Parse streamed argument text into the value we store in the log.
pub fn parse_arguments(raw: &str) -> Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Value::Object(serde_json::Map::new());
    }
    serde_json::from_str(trimmed).unwrap_or_else(|_| Value::String(raw.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str, args: Value) -> LoggedToolCall {
        LoggedToolCall {
            id: id.into(),
            name: "bash".into(),
            args,
        }
    }

    #[test]
    fn assistant_with_only_tools_has_null_content() {
        let msg = ChatMessage::assistant("", &[call("c1", json!({"command": "ls"}))]);
        let value = serde_json::to_value(&msg).unwrap();
        assert!(value["content"].is_null());
        assert_eq!(value["role"], "assistant");
        assert_eq!(value["tool_calls"][0]["type"], "function");
        assert_eq!(value["tool_calls"][0]["function"]["name"], "bash");
        assert_eq!(
            value["tool_calls"][0]["function"]["arguments"],
            "{\"command\":\"ls\"}"
        );
    }

    #[test]
    fn assistant_with_text_and_calls_keeps_text() {
        let msg = ChatMessage::assistant("working", &[call("c1", json!({}))]);
        assert_eq!(msg.content.as_deref(), Some("working"));
    }

    #[test]
    fn tool_message_carries_call_id() {
        let value = serde_json::to_value(ChatMessage::tool("c9", "ok")).unwrap();
        assert_eq!(value["role"], "tool");
        assert_eq!(value["tool_call_id"], "c9");
        assert_eq!(value["content"], "ok");
    }

    #[test]
    fn string_arguments_are_not_double_encoded() {
        let c = call("c1", Value::String("{\"command\":\"pwd\"}".into()));
        assert_eq!(arguments_string(&c.args), "{\"command\":\"pwd\"}");
    }

    #[test]
    fn parse_arguments_handles_empty_garbage_and_objects() {
        assert_eq!(parse_arguments(""), json!({}));
        assert_eq!(parse_arguments("   "), json!({}));
        assert_eq!(parse_arguments("{\"a\":1}"), json!({"a": 1}));
        assert_eq!(parse_arguments("not json"), json!("not json"));
    }
}
