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
    // The system message is frozen in `session_started` and never compacted
    // away, so it is read before the range logic.
    for event in events {
        if let EventBody::SessionStarted { system, .. } = &event.body {
            folded.system = system.text.resolve_text(session_dir)?;
            break;
        }
    }

    // Which seqs a compaction replaced, and which of those compactions a later
    // one replaced in turn. A compaction renders as "summary + pins" only where
    // it still stands: a second compaction re-summarises the first one and
    // re-pins the human messages it kept.
    let compactions: Vec<u64> = events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::Compacted { through_seq, .. } => Some(*through_seq),
            _ => None,
        })
        .collect();
    let replaced = |seq: u64| compactions.iter().any(|through| seq <= *through);
    let by_seq: std::collections::BTreeMap<u64, &LogEvent> =
        events.iter().map(|event| (event.seq, event)).collect();

    for event in events {
        // A compaction renders where it was written, unless a later one
        // re-summarised it; `replaced` is false for its own seq (a compaction
        // always covers events before itself).
        if let EventBody::Compacted {
            summary,
            pinned_seqs,
            ..
        } = &event.body
        {
            if !replaced(event.seq) {
                let text = summary.resolve_text(session_dir)?;
                folded
                    .messages
                    .push(ChatMessage::user(&compaction_message(&text)));
                // Pins render in log order whatever order the event lists them
                // in: the conversation stays chronological.
                let mut pins = pinned_seqs.clone();
                pins.sort_unstable();
                for pin in pins {
                    if let Some(pinned) = by_seq.get(&pin) {
                        render(&mut folded, pinned, session_dir)?;
                    }
                }
            }
            continue;
        }
        if replaced(event.seq) {
            continue;
        }
        render(&mut folded, event, session_dir)?;
    }
    Ok(folded)
}

/// How a compaction's summary reaches the model: one user message at the head
/// of the retained conversation. The pins follow it, verbatim.
pub fn compaction_message(summary: &str) -> String {
    format!("[kelir compacted the earlier conversation]\n\n{summary}")
}

fn render(folded: &mut Folded, event: &LogEvent, session_dir: &Path) -> Result<(), String> {
    match &event.body {
        EventBody::SessionStarted { .. } | EventBody::Compacted { .. } => {}
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
                return Ok(());
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
    Ok(())
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
    fn a_compaction_replaces_its_range_and_pins_verbatim_in_order() {
        fn user_from(seq: u64, text: &str, from: &str) -> LogEvent {
            LogEvent::new(
                seq,
                EventBody::User {
                    text: text.into(),
                    from: from.into(),
                    delivery: "now".into(),
                    attachments: vec![],
                },
            )
        }
        fn assistant(seq: u64, text: &str) -> LogEvent {
            LogEvent::new(
                seq,
                EventBody::Assistant {
                    step: seq as u32,
                    text: text.into(),
                    reasoning: None,
                    tool_calls: vec![],
                    finish_reason: Some("stop".into()),
                },
            )
        }
        let events = vec![
            started(),
            user_from(2, "the task", "cli"),
            user_from(3, "a hint from a job", "job"),
            assistant(4, "step one"),
            user_from(5, "human steering", "ctl"),
            assistant(6, "step two"),
            LogEvent::new(
                7,
                EventBody::Compacted {
                    through_seq: 6,
                    summary: "SUMMARY".into(),
                    pinned_seqs: vec![5, 2],
                    by: "agent".into(),
                    tokens_before: 900,
                    tokens_after: 90,
                    cache_restart: true,
                },
            ),
            user_from(8, "after the compaction", "tui"),
        ];
        let folded = fold(&events, Path::new("")).unwrap();
        assert_eq!(folded.system, "SYS", "the system prefix survives");
        let contents: Vec<&str> = folded
            .messages
            .iter()
            .map(|message| message.content.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(contents.len(), 4, "{contents:?}");
        assert!(contents[0].contains("SUMMARY"), "{}", contents[0]);
        // Pins keep their log order (2 then 5), verbatim.
        assert_eq!(contents[1], "the task");
        assert_eq!(contents[2], "human steering");
        assert_eq!(contents[3], "after the compaction");
        // Everything in the range that was not pinned is gone.
        assert!(!contents.iter().any(|text| text.contains("step one")));
        assert!(!contents.iter().any(|text| text.contains("step two")));
        assert!(
            !contents
                .iter()
                .any(|text| text.contains("a hint from a job"))
        );
    }

    #[test]
    fn a_second_compaction_supersedes_the_first_and_re_pins() {
        let events = vec![
            started(),
            user("the task"),
            LogEvent::new(
                3,
                EventBody::Assistant {
                    step: 1,
                    text: "one".into(),
                    reasoning: None,
                    tool_calls: vec![],
                    finish_reason: Some("stop".into()),
                },
            ),
            LogEvent::new(
                4,
                EventBody::Compacted {
                    through_seq: 3,
                    summary: "FIRST".into(),
                    pinned_seqs: vec![2],
                    by: "auto".into(),
                    tokens_before: 10,
                    tokens_after: 5,
                    cache_restart: true,
                },
            ),
            LogEvent::new(
                5,
                EventBody::User {
                    text: "more".into(),
                    from: "tui".into(),
                    delivery: "now".into(),
                    attachments: vec![],
                },
            ),
            LogEvent::new(
                6,
                EventBody::Assistant {
                    step: 2,
                    text: "two".into(),
                    reasoning: None,
                    tool_calls: vec![],
                    finish_reason: Some("stop".into()),
                },
            ),
            LogEvent::new(
                7,
                EventBody::Compacted {
                    through_seq: 6,
                    summary: "SECOND".into(),
                    pinned_seqs: vec![2, 5],
                    by: "user".into(),
                    tokens_before: 20,
                    tokens_after: 6,
                    cache_restart: true,
                },
            ),
        ];
        let folded = fold(&events, Path::new("")).unwrap();
        let contents: Vec<&str> = folded
            .messages
            .iter()
            .map(|message| message.content.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(contents.len(), 3, "{contents:?}");
        assert!(contents[0].contains("SECOND"));
        assert!(!contents[0].contains("FIRST"), "the old summary is gone");
        assert_eq!(contents[1], "the task");
        assert_eq!(contents[2], "more");
        assert_eq!(
            folded
                .messages
                .iter()
                .filter(|message| message.content.as_deref() == Some("the task"))
                .count(),
            1,
            "a pin is rendered once"
        );
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
