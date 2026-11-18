//! `fold(events.jsonl)`: the one code path that turns the log into context.
//!
//! Live and on resume the request is built from this, never from memory.

use crate::chat::ChatMessage;
use crate::event::{EventBody, LogEvent};

#[derive(Debug, Clone, Default)]
pub struct Folded {
    /// The frozen system text from `session_started`.
    pub system: String,
    /// The conversation, system message excluded.
    pub messages: Vec<ChatMessage>,
}

impl Folded {
    /// System message first, then every rendered message. This is exactly what
    /// goes on the wire, so the prefix only ever grows.
    pub fn request_messages(&self) -> Vec<ChatMessage> {
        let mut out = Vec::with_capacity(self.messages.len() + 1);
        out.push(ChatMessage::system(&self.system));
        out.extend(self.messages.iter().cloned());
        out
    }
}

pub fn fold(events: &[LogEvent]) -> Folded {
    let mut folded = Folded::default();
    for event in events {
        match &event.body {
            EventBody::SessionStarted { system, .. } => {
                folded.system = system.text.clone();
            }
            EventBody::User { text, .. } => {
                folded.messages.push(ChatMessage::user(text));
            }
            EventBody::Source {
                kind,
                name,
                text,
                path,
            } => {
                let body = text.clone().or_else(|| path.clone()).unwrap_or_default();
                folded.messages.push(ChatMessage::user(&format!(
                    "[source: {kind} {name}]\n{body}"
                )));
            }
            EventBody::Assistant {
                text, tool_calls, ..
            } => {
                if text.is_empty() && tool_calls.is_empty() {
                    continue;
                }
                folded
                    .messages
                    .push(ChatMessage::assistant(text, tool_calls));
            }
            EventBody::ToolSettled {
                call_id, result, ..
            } => {
                folded.messages.push(ChatMessage::tool(call_id, result));
            }
            _ => {}
        }
    }
    folded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{LoggedToolCall, SystemInfo};
    use serde_json::json;

    fn started() -> LogEvent {
        LogEvent::new(
            1,
            EventBody::SessionStarted {
                id: "ses_1".into(),
                parent: None,
                depth: 0,
                cwd: "/tmp".into(),
                os: "linux".into(),
                kelir_version: "0.1.0".into(),
                provider: "openrouter".into(),
                model: "m".into(),
                system: SystemInfo {
                    text: "SYS".into(),
                    hash: "h".into(),
                },
                sources: vec![],
                tools: vec!["bash".into()],
            },
        )
    }

    #[test]
    fn renders_system_then_conversation() {
        let events = vec![
            started(),
            LogEvent::new(
                2,
                EventBody::User {
                    text: "task".into(),
                    from: "cli".into(),
                    delivery: "now".into(),
                    attachments: vec![],
                },
            ),
            LogEvent::new(
                3,
                EventBody::Assistant {
                    step: 1,
                    text: "".into(),
                    reasoning: None,
                    tool_calls: vec![LoggedToolCall {
                        id: "c1".into(),
                        name: "bash".into(),
                        args: json!({"command": "echo hi"}),
                    }],
                    finish_reason: Some("tool_calls".into()),
                },
            ),
            LogEvent::new(
                4,
                EventBody::ToolSettled {
                    call_id: "c1".into(),
                    outcome: "ok".into(),
                    exit_code: Some(0),
                    bytes: 3,
                    lines: 1,
                    out_path: "out".into(),
                    spilled: false,
                    result: "hi\n".into(),
                },
            ),
            LogEvent::new(
                5,
                EventBody::Assistant {
                    step: 2,
                    text: "done".into(),
                    reasoning: None,
                    tool_calls: vec![],
                    finish_reason: Some("stop".into()),
                },
            ),
        ];
        let folded = fold(&events);
        assert_eq!(folded.system, "SYS");
        let messages = folded.request_messages();
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[0].content.as_deref(), Some("SYS"));
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[2].role, "assistant");
        assert!(messages[2].content.is_none());
        assert_eq!(messages[2].tool_calls.as_ref().unwrap()[0].id, "c1");
        assert_eq!(messages[3].role, "tool");
        assert_eq!(messages[3].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(messages[3].content.as_deref(), Some("hi\n"));
        assert_eq!(messages[4].role, "assistant");
        assert_eq!(messages[4].content.as_deref(), Some("done"));
    }

    #[test]
    fn empty_assistant_events_are_skipped() {
        let events = vec![
            started(),
            LogEvent::new(
                2,
                EventBody::Assistant {
                    step: 1,
                    text: "".into(),
                    reasoning: None,
                    tool_calls: vec![],
                    finish_reason: None,
                },
            ),
        ];
        assert_eq!(fold(&events).messages.len(), 0);
    }

    #[test]
    fn sources_render_at_the_tail() {
        let events = vec![
            started(),
            LogEvent::new(
                2,
                EventBody::User {
                    text: "a".into(),
                    from: "cli".into(),
                    delivery: "now".into(),
                    attachments: vec![],
                },
            ),
            LogEvent::new(
                3,
                EventBody::Source {
                    kind: "hint".into(),
                    name: "hint.md".into(),
                    text: Some("consider rg".into()),
                    path: None,
                },
            ),
        ];
        let folded = fold(&events);
        let last = folded.messages.last().unwrap();
        assert_eq!(last.role, "user");
        let content = last.content.as_deref().unwrap();
        assert!(content.contains("[source: hint hint.md]"));
        assert!(content.contains("consider rg"));
    }

    #[test]
    fn usage_and_meta_events_do_not_render() {
        let events = vec![
            started(),
            LogEvent::new(
                2,
                EventBody::Usage {
                    step: 1,
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    cached_tokens: 1,
                    cache_write_tokens: 0,
                    cost: 0.0,
                    allowance: None,
                },
            ),
            LogEvent::new(
                3,
                EventBody::StepStarted {
                    step: 2,
                    provider: "p".into(),
                    model: "m".into(),
                    request_hash: "0".into(),
                },
            ),
        ];
        assert!(fold(&events).messages.is_empty());
    }
}
