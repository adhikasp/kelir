//! Assemble a Chat Completions stream into one assistant turn.
//!
//! Deltas arrive as text, reasoning and tool-call fragments spread across
//! chunks; tool calls are keyed by their `index` and concatenated. The final
//! usage-only chunk (sent because `stream_options.include_usage` is on) is
//! captured here, including the cache fields.

use serde_json::Value;

use crate::chat::parse_arguments;
use crate::event::LoggedToolCall;
use crate::sse::SseEvent;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost: f64,
    pub allowance: Option<Value>,
}

#[derive(Debug, Default, Clone)]
struct ToolAcc {
    index: usize,
    id: String,
    name: String,
    arguments: String,
}

#[derive(Debug, Default)]
pub struct Assembler {
    pub text: String,
    pub reasoning: String,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
    pub done: bool,
    reasoning_field: Option<String>,
    calls: Vec<ToolAcc>,
}

impl Assembler {
    /// `reasoning_field` comes from the provider config; when unset, both
    /// `reasoning` and `reasoning_content` are tried.
    pub fn new(reasoning_field: Option<String>) -> Self {
        Self {
            reasoning_field,
            ..Default::default()
        }
    }

    /// Feed one SSE event. `[DONE]` marks the end of the stream.
    pub fn feed_event(&mut self, event: &SseEvent) -> Result<(), String> {
        let data = event.data.trim();
        if data.is_empty() {
            return Ok(());
        }
        if data == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let value: Value = serde_json::from_str(data)
            .map_err(|_| format!("invalid JSON in stream chunk: {}", truncate(data, 200)))?;
        self.feed_json(&value);
        Ok(())
    }

    pub fn feed_json(&mut self, value: &Value) {
        if let Some(usage) = value.get("usage").filter(|u| !u.is_null()) {
            self.usage = Some(parse_usage(usage));
        }

        let Some(choice) = value
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
        else {
            return;
        };

        if let Some(delta) = choice.get("delta") {
            if let Some(text) = text_of(delta.get("content")) {
                self.text.push_str(&text);
            }
            let reasoning = match &self.reasoning_field {
                Some(field) => text_of(delta.get(field)),
                None => text_of(delta.get("reasoning"))
                    .or_else(|| text_of(delta.get("reasoning_content"))),
            };
            if let Some(reasoning) = reasoning {
                self.reasoning.push_str(&reasoning);
            }
            if let Some(calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for (position, call) in calls.iter().enumerate() {
                    let index = call
                        .get("index")
                        .and_then(|v| v.as_u64())
                        .map(|v| v as usize)
                        .unwrap_or(position);
                    self.apply_tool_delta(index, call);
                }
            }
        }

        if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            self.finish_reason = Some(reason.to_string());
        }
    }

    fn apply_tool_delta(&mut self, index: usize, call: &Value) {
        let slot = match self.calls.iter().position(|c| c.index == index) {
            Some(pos) => pos,
            None => {
                self.calls.push(ToolAcc {
                    index,
                    ..Default::default()
                });
                self.calls.len() - 1
            }
        };
        if let Some(id) = call.get("id").and_then(|v| v.as_str()) {
            if !id.is_empty() {
                self.calls[slot].id.push_str(id);
            }
        }
        if let Some(function) = call.get("function") {
            if let Some(name) = function.get("name").and_then(|v| v.as_str()) {
                self.calls[slot].name.push_str(name);
            }
            if let Some(arguments) = function.get("arguments").and_then(|v| v.as_str()) {
                self.calls[slot].arguments.push_str(arguments);
            }
        }
    }

    /// The assembled tool calls, in the order the model emitted them.
    pub fn tool_calls(&self) -> Vec<LoggedToolCall> {
        let mut calls = self.calls.clone();
        calls.sort_by_key(|c| c.index);
        calls
            .into_iter()
            .enumerate()
            .map(|(position, call)| LoggedToolCall {
                id: if call.id.is_empty() {
                    format!("call_{position}")
                } else {
                    call.id
                },
                name: call.name,
                args: parse_arguments(&call.arguments),
            })
            .collect()
    }

    pub fn has_content(&self) -> bool {
        !self.text.is_empty() || !self.reasoning.is_empty() || !self.calls.is_empty()
    }
}

fn text_of(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                    out.push_str(text);
                }
            }
            Some(out)
        }
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn parse_usage(usage: &Value) -> Usage {
    let details = usage.get("prompt_tokens_details");
    let cached = details
        .and_then(|d| d.get("cached_tokens"))
        .or_else(|| usage.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_write = details
        .and_then(|d| d.get("cache_write_tokens"))
        .or_else(|| usage.get("cache_write_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cost = usage
        .get("cost")
        .or_else(|| usage.get("total_cost"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    Usage {
        prompt_tokens: usage
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        completion_tokens: usage
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cached_tokens: cached,
        cache_write_tokens: cache_write,
        cost,
        allowance: usage.get("allowance").filter(|v| !v.is_null()).cloned(),
    }
}

fn truncate(text: &str, max: usize) -> String {
    crate::util::truncate_chars(text, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn feed(asm: &mut Assembler, value: Value) {
        asm.feed_json(&value);
    }

    #[test]
    fn text_deltas_concatenate() {
        let mut asm = Assembler::new(None);
        feed(&mut asm, json!({"choices":[{"delta":{"content":"He"}}]}));
        feed(&mut asm, json!({"choices":[{"delta":{"content":"llo"}}]}));
        assert_eq!(asm.text, "Hello");
    }

    #[test]
    fn text_parts_array_is_joined() {
        let mut asm = Assembler::new(None);
        feed(
            &mut asm,
            json!({"choices":[{"delta":{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}}]}),
        );
        assert_eq!(asm.text, "ab");
    }

    #[test]
    fn tool_call_fragments_assemble_across_chunks() {
        let mut asm = Assembler::new(None);
        feed(
            &mut asm,
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"bash","arguments":""}}]}}]}),
        );
        feed(
            &mut asm,
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"comm"}}]}}]}),
        );
        feed(
            &mut asm,
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"and\":\"ls\"}"}}]}}]}),
        );
        let calls = asm.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[0].args, json!({"command": "ls"}));
    }

    #[test]
    fn two_tool_calls_keep_model_order() {
        let mut asm = Assembler::new(None);
        feed(
            &mut asm,
            json!({"choices":[{"delta":{"tool_calls":[
                {"index":1,"id":"b","function":{"name":"bash","arguments":"{}"}},
                {"index":0,"id":"a","function":{"name":"bash","arguments":"{}"}}
            ]}}]}),
        );
        let calls = asm.tool_calls();
        assert_eq!(calls[0].id, "a");
        assert_eq!(calls[1].id, "b");
    }

    #[test]
    fn name_split_across_chunks_is_joined() {
        let mut asm = Assembler::new(None);
        feed(
            &mut asm,
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"x","function":{"name":"ba"}}]}}]}),
        );
        feed(
            &mut asm,
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"sh","arguments":"{}"}}]}}]}),
        );
        assert_eq!(asm.tool_calls()[0].name, "bash");
    }

    #[test]
    fn finish_reason_is_captured() {
        let mut asm = Assembler::new(None);
        feed(
            &mut asm,
            json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
        );
        assert_eq!(asm.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn usage_only_final_chunk_is_captured() {
        let mut asm = Assembler::new(None);
        feed(
            &mut asm,
            json!({"choices":[],"usage":{
                "prompt_tokens": 120,
                "completion_tokens": 8,
                "prompt_tokens_details": {"cached_tokens": 96, "cache_write_tokens": 24},
                "cost": 0.0042
            }}),
        );
        let usage = asm.usage.expect("usage");
        assert_eq!(usage.prompt_tokens, 120);
        assert_eq!(usage.completion_tokens, 8);
        assert_eq!(usage.cached_tokens, 96);
        assert_eq!(usage.cache_write_tokens, 24);
        assert!((usage.cost - 0.0042).abs() < 1e-9);
        assert_eq!(asm.text, "");
    }

    #[test]
    fn openrouter_total_cost_and_allowance_are_read() {
        let mut asm = Assembler::new(None);
        feed(
            &mut asm,
            json!({"usage":{"prompt_tokens":1,"completion_tokens":2,"total_cost":0.5,"allowance":{"left":3}}}),
        );
        let usage = asm.usage.unwrap();
        assert!((usage.cost - 0.5).abs() < 1e-9);
        assert_eq!(usage.allowance, Some(json!({"left": 3})));
    }

    #[test]
    fn configured_reasoning_field_wins() {
        let mut asm = Assembler::new(Some("thinking".into()));
        feed(
            &mut asm,
            json!({"choices":[{"delta":{"thinking":"hmm","reasoning":"no"}}]}),
        );
        assert_eq!(asm.reasoning, "hmm");
    }

    #[test]
    fn reasoning_content_fallback() {
        let mut asm = Assembler::new(None);
        feed(
            &mut asm,
            json!({"choices":[{"delta":{"reasoning_content":"deep"}}]}),
        );
        assert_eq!(asm.reasoning, "deep");
    }

    #[test]
    fn done_event_marks_completion() {
        let mut asm = Assembler::new(None);
        let event = SseEvent {
            event: None,
            data: "[DONE]".into(),
        };
        asm.feed_event(&event).unwrap();
        assert!(asm.done);
    }

    #[test]
    fn invalid_json_is_an_error() {
        let event = SseEvent {
            event: None,
            data: "{not json".into(),
        };
        let mut asm = Assembler::new(None);
        assert!(asm.feed_event(&event).is_err());
    }
}
