//! Chat messages in the OpenAI Chat Completions shape.
//!
//! The fold produces these; the request builder serialises them unchanged, so
//! a request's message prefix is byte-identical from one step to the next.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A tool call with its arguments resolved (never spilled): what the loop
/// executes and what the request carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
}

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
    /// Image parts, when this message carries any. The OpenAI Chat Completions
    /// shape puts them in `content` as a list, so the wire form is built in
    /// [`ChatMessage::to_wire`]; `content` above stays the text, which is what
    /// every other message and the whole existing code path use.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImagePart>,
}

/// One image in a message: a `data:` URL and its MIME type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImagePart {
    pub mime: String,
    pub data_url: String,
}

impl ImagePart {
    pub fn png(data_url: impl Into<String>) -> Self {
        Self {
            mime: "image/png".into(),
            data_url: data_url.into(),
        }
    }
}

impl ChatMessage {
    pub fn system(text: &str) -> Self {
        Self {
            role: "system".into(),
            content: Some(text.to_string()),
            tool_calls: None,
            tool_call_id: None,
            images: Vec::new(),
        }
    }

    pub fn user(text: &str) -> Self {
        Self {
            role: "user".into(),
            content: Some(text.to_string()),
            tool_calls: None,
            tool_call_id: None,
            images: Vec::new(),
        }
    }

    /// A user message that carries images: the text (when any) and one image
    /// part per attachment.
    pub fn user_with_images(text: &str, images: Vec<ImagePart>) -> Self {
        Self {
            role: "user".into(),
            content: (!text.is_empty()).then(|| text.to_string()),
            tool_calls: None,
            tool_call_id: None,
            images,
        }
    }

    /// The wire form: `content` stays a string unless the message carries
    /// images, in which case it becomes the list of text and image parts that
    /// OpenAI-compatible providers expect. A message with neither text nor
    /// images keeps `content: null`.
    pub fn to_wire(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or(Value::Null);
        if self.images.is_empty() {
            return value;
        }
        let mut parts = Vec::with_capacity(self.images.len() + 1);
        if let Some(text) = &self.content {
            parts.push(serde_json::json!({"type": "text", "text": text}));
        }
        for image in &self.images {
            parts.push(serde_json::json!({
                "type": "image_url",
                "image_url": {"url": image.data_url},
            }));
        }
        value["content"] = Value::Array(parts);
        // `images` is kelir's own field; the wire has it inside `content`.
        if let Some(object) = value.as_object_mut() {
            object.remove("images");
        }
        value
    }

    /// An assistant turn. A turn that only calls tools has `content: null`,
    /// which OpenAI-compatible APIs accept and expect.
    pub fn assistant(text: &str, calls: &[ToolCall]) -> Self {
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
            images: Vec::new(),
        }
    }

    pub fn tool(call_id: &str, content: &str) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: Some(call_id.to_string()),
            images: Vec::new(),
        }
    }
}

impl From<&ToolCall> for ApiToolCall {
    fn from(call: &ToolCall) -> Self {
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

    fn call(id: &str, args: Value) -> ToolCall {
        ToolCall {
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
