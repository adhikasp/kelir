//! `fold(events.jsonl)`: the one code path that turns the log into context.
//!
//! Live and on resume the request is built from this, never from memory.
//! Spilled payloads are read back here, so a request built from blobs is
//! byte-identical to one built from inline values (the cache prefix cannot
//! change because the log got large).

use std::path::Path;

use crate::chat::{ChatMessage, ToolCall};
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

pub fn fold(events: &[LogEvent], session_dir: &Path) -> Result<Folded, String> {
    let mut folded = Folded::default();
    for event in events {
        match &event.body {
            EventBody::SessionStarted { system, .. } => {
                folded.system = system.text.resolve_text(session_dir)?;
            }
            EventBody::User { text, .. } => {
                folded
                    .messages
                    .push(ChatMessage::user(&text.resolve_text(session_dir)?));
            }
            EventBody::Source {
                kind,
                name,
                text,
                path,
            } => {
                let body = match text {
                    Some(text) => text.resolve_text(session_dir)?,
                    None => path.clone().unwrap_or_default(),
                };
                folded.messages.push(ChatMessage::user(&format!(
                    "[source: {kind} {name}]\n{body}"
                )));
            }
            EventBody::Assistant {
                text, tool_calls, ..
            } => {
                let text = text.resolve_text(session_dir)?;
                let mut calls = Vec::with_capacity(tool_calls.len());
                for call in tool_calls {
                    calls.push(ToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        args: call.args.resolve(session_dir)?,
                    });
                }
                if text.is_empty() && calls.is_empty() {
                    continue;
                }
                folded.messages.push(ChatMessage::assistant(&text, &calls));
            }
            EventBody::ToolSettled {
                call_id, result, ..
            } => {
                folded.messages.push(ChatMessage::tool(
                    call_id,
                    &result.resolve_text(session_dir)?,
                ));
            }
            _ => {}
        }
    }
    Ok(folded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{
        Blobs, EventBody, LogEvent, LoggedToolCall, Payload, SystemInfo, spill_payloads,
    };
    use serde_json::json;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kelir-fold-{}-{name}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

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

    fn user(text: &str) -> LogEvent {
        LogEvent::new(
            2,
            EventBody::User {
                text: text.into(),
                from: "cli".into(),
                delivery: "now".into(),
                attachments: vec![],
            },
        )
    }

    #[test]
    fn renders_system_then_conversation() {
        let events = vec![
            started(),
            user("task"),
            LogEvent::new(
                3,
                EventBody::Assistant {
                    step: 1,
                    text: "".into(),
                    reasoning: None,
                    tool_calls: vec![LoggedToolCall {
                        id: "c1".into(),
                        name: "bash".into(),
                        args: json!({"command": "echo hi"}).into(),
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
        let folded = fold(&events, Path::new("")).unwrap();
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
        assert_eq!(fold(&events, Path::new("")).unwrap().messages.len(), 0);
    }

    #[test]
    fn sources_render_at_the_tail() {
        let events = vec![
            started(),
            user("a"),
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
        let folded = fold(&events, Path::new("")).unwrap();
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
        assert!(fold(&events, Path::new("")).unwrap().messages.is_empty());
    }

    #[test]
    fn a_spilled_tool_argument_folds_back_to_identical_request_bytes() {
        let dir = temp_dir("args");
        let big = format!("{}\n", "y".repeat(20_000));
        let args = json!({"path": "big.txt", "old": "", "new": big});

        let inline_body = EventBody::Assistant {
            step: 1,
            text: "".into(),
            reasoning: None,
            tool_calls: vec![LoggedToolCall {
                id: "c1".into(),
                name: "edit".into(),
                args: Payload::value(args.clone()),
            }],
            finish_reason: Some("tool_calls".into()),
        };
        let spilled_body = spill_payloads(inline_body.clone(), &Blobs::new(&dir), 3).unwrap();
        assert_ne!(inline_body, spilled_body);

        let inline_log = vec![started(), user("go"), LogEvent::new(3, inline_body)];
        let spilled_log = vec![started(), user("go"), LogEvent::new(3, spilled_body)];

        let inline = fold(&inline_log, &dir).unwrap();
        let spilled = fold(&spilled_log, &dir).unwrap();
        assert_eq!(
            serde_json::to_vec(&inline.request_messages()).unwrap(),
            serde_json::to_vec(&spilled.request_messages()).unwrap()
        );
        // The log line stays small even though the call is 20 KB.
        assert!(serde_json::to_string(&spilled_log[2]).unwrap().len() < 2_000);
    }

    #[test]
    fn a_spilled_text_field_folds_back_verbatim() {
        let dir = temp_dir("text");
        let big = "line\n".repeat(4_000);
        let body = EventBody::User {
            text: Payload::text(big.clone()),
            from: "cli".into(),
            delivery: "now".into(),
            attachments: vec![],
        };
        let spilled = spill_payloads(body, &Blobs::new(&dir), 2).unwrap();
        let log = vec![started(), LogEvent::new(2, spilled)];
        let folded = fold(&log, &dir).unwrap();
        assert_eq!(folded.messages[0].content.as_deref(), Some(big.as_str()));
        assert!(serde_json::to_string(&log[1]).unwrap().len() < 2_000);
    }

    #[test]
    fn a_missing_blob_is_an_error_not_a_silent_truncation() {
        let dir = temp_dir("missing");
        let log = vec![
            started(),
            LogEvent::new(
                2,
                EventBody::Assistant {
                    step: 1,
                    text: "".into(),
                    reasoning: None,
                    tool_calls: vec![LoggedToolCall {
                        id: "c1".into(),
                        name: "bash".into(),
                        args: Payload::Blob(crate::event::BlobRef {
                            spilled: true,
                            path: "blobs/2-tool_calls.0.args".into(),
                            bytes: 10,
                            preview: "…".into(),
                        }),
                    }],
                    finish_reason: None,
                },
            ),
        ];
        let error = fold(&log, &dir).unwrap_err();
        assert!(error.contains("cannot read spilled payload"), "{error}");
    }
}
