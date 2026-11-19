//! `fold(events.jsonl)`: the one code path that turns the log into context.
//!
//! Live and on resume the request is built from this, never from memory.
//! Spilled payloads are read back here, so a request built from blobs is
//! byte-identical to one built from inline values (the cache prefix cannot
//! change because the log got large).

use std::collections::{BTreeMap, HashSet};
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

    // The compactions in this log, and which of them a later one supersedes.
    // A compaction renders as "summary + pins" only while it stands: a later
    // one that covers its range (or that reaches over the compaction event
    // itself) has already summarised it, so the old summary and its pins are
    // gone.
    struct Rule<'a> {
        seq: u64,
        through: u64,
        body: &'a EventBody,
    }
    let mut rules: Vec<Rule> = events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::Compacted { through_seq, .. } => Some(Rule {
                seq: event.seq,
                through: *through_seq,
                body: &event.body,
            }),
            _ => None,
        })
        .collect();
    rules.sort_by_key(|rule| rule.seq);
    let mut superseded = vec![false; rules.len()];
    for (index, rule) in rules.iter().enumerate() {
        for later in &rules[index + 1..] {
            if later.through >= rule.through || later.through >= rule.seq {
                superseded[index] = true;
            }
        }
    }
    let mut standing: Vec<&Rule> = rules
        .iter()
        .enumerate()
        .filter(|(index, _)| !superseded[*index])
        .map(|(_, rule)| rule)
        .collect();
    standing.sort_by_key(|rule| rule.through);

    let by_seq: BTreeMap<u64, &LogEvent> = events.iter().map(|event| (event.seq, event)).collect();
    // The log is normally in seq order; a transcript builder may hand in a
    // later `compacted` event beside an earlier range, so order it here.
    let mut ordered: Vec<&LogEvent> = events.iter().collect();
    ordered.sort_by_key(|event| event.seq);
    let mut rendered_pins: HashSet<u64> = HashSet::new();

    // A compaction renders at `through_seq` — where its range ends — not where
    // its event was written. That keeps the summary and the pins *before* the
    // kept tail, so the request reads `[system, summary, pins, tail, …later]`
    // and its last message is the newest event, never an old pin.
    let mut next = 0usize;
    for event in ordered {
        while next < standing.len() && standing[next].through < event.seq {
            render_compaction(
                &mut folded,
                standing[next].body,
                &by_seq,
                &mut rendered_pins,
                session_dir,
            )?;
            next += 1;
        }
        // A compaction event never renders where it sits: it either rendered
        // at its range above, or a later compaction superseded it.
        if matches!(event.body, EventBody::Compacted { .. }) {
            continue;
        }
        if standing.iter().any(|rule| event.seq <= rule.through) {
            continue;
        }
        render(&mut folded, event, session_dir)?;
    }
    while next < standing.len() {
        render_compaction(
            &mut folded,
            standing[next].body,
            &by_seq,
            &mut rendered_pins,
            session_dir,
        )?;
        next += 1;
    }
    Ok(folded)
}

/// Render one standing compaction: its summary, then its pins verbatim in log
/// order. A pin is rendered at most once, even when two compactions name it.
fn render_compaction(
    folded: &mut Folded,
    body: &EventBody,
    by_seq: &BTreeMap<u64, &LogEvent>,
    rendered_pins: &mut HashSet<u64>,
    session_dir: &Path,
) -> Result<(), String> {
    let EventBody::Compacted {
        summary,
        pinned_seqs,
        ..
    } = body
    else {
        return Ok(());
    };
    let text = summary.resolve_text(session_dir)?;
    folded
        .messages
        .push(ChatMessage::user(&compaction_message(&text)));
    let mut pins = pinned_seqs.clone();
    pins.sort_unstable();
    for pin in pins {
        if !rendered_pins.insert(pin) {
            continue;
        }
        if let Some(pinned) = by_seq.get(&pin) {
            render(folded, pinned, session_dir)?;
        }
    }
    Ok(())
}

/// The events a compaction of `through_seq` is summarised from: everything in
/// the range, plus every `compacted` event whose own range is inside it. The
/// latter carries the earlier summary, so a second compaction summarises the
/// first one instead of losing it.
pub fn range_events(events: &[LogEvent], through_seq: u64) -> Vec<LogEvent> {
    events
        .iter()
        .filter(|event| match &event.body {
            EventBody::Compacted {
                through_seq: inner, ..
            } => *inner <= through_seq,
            _ => event.seq <= through_seq,
        })
        .cloned()
        .collect()
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
            if kind == "attachment" {
                // An image queued by `kelir view`: a user message holding one
                // image part, at the tail. The log stores the relative path,
                // never the bytes, so the data URL is built here, at request
                // time, from the file the session still has.
                let rel = path.clone().unwrap_or_else(|| name.clone());
                let file = session_dir.join(&rel);
                let bytes = std::fs::read(&file).map_err(|error| {
                    format!(
                        "[source: attachment {rel}] cannot read {}: {error}",
                        crate::util::path_string(&file)
                    )
                })?;
                let mime = crate::attachments::mime_of(&file);
                folded.messages.push(ChatMessage::user_with_images(
                    "",
                    vec![crate::chat::ImagePart {
                        mime: mime.to_string(),
                        data_url: crate::attachments::data_url(mime, &bytes),
                    }],
                ));
                return Ok(());
            }
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
                reasoning_effort: None,
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

    /// An attachment is a user message holding an image part, built from the
    /// file at request time: the log never holds the bytes.
    #[test]
    fn an_attachment_source_renders_as_one_image_part_at_the_tail() {
        let dir = temp_dir("attachment");
        std::fs::create_dir_all(dir.join("attachments")).unwrap();
        // A minimal PNG header (signature, IHDR, 1x1): the fold does not
        // decode it, it only base64-encodes it.
        let mut png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1u32.to_be_bytes());
        png.extend_from_slice(&1u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        std::fs::write(dir.join("attachments/1-x.png"), png).unwrap();
        let events = vec![
            started(),
            user("what is in this?"),
            LogEvent::new(
                3,
                EventBody::Source {
                    kind: "attachment".into(),
                    name: "attachments/1-x.png".into(),
                    text: None,
                    path: Some("attachments/1-x.png".into()),
                },
            ),
        ];
        let folded = fold(&events, &dir).unwrap();
        let last = folded.messages.last().unwrap();
        assert_eq!(last.role, "user");
        assert!(last.content.is_none(), "an attachment carries no text");
        assert_eq!(last.images.len(), 1);
        assert_eq!(last.images[0].mime, "image/png");
        assert!(
            last.images[0]
                .data_url
                .starts_with("data:image/png;base64,")
        );
        // The wire form is the list of parts, and it has no `images` field.
        let wire = last.to_wire();
        assert!(wire["content"].is_array(), "{wire}");
        assert_eq!(wire["content"][0]["type"], "image_url");
        assert!(wire.get("images").is_none(), "{wire}");
        // Every earlier message keeps its plain string content: the cached
        // prefix is untouched.
        let earlier = folded
            .messages
            .iter()
            .find(|message| message.content.as_deref() == Some("what is in this?"))
            .expect("the earlier user message");
        assert!(
            earlier.to_wire()["content"].is_string(),
            "an earlier message stays a plain string"
        );
        assert!(earlier.images.is_empty());
    }

    #[test]
    fn an_attachment_with_a_missing_file_is_an_error_not_a_silent_skip() {
        let dir = temp_dir("attachment-missing");
        let events = vec![
            started(),
            LogEvent::new(
                2,
                EventBody::Source {
                    kind: "attachment".into(),
                    name: "attachments/gone.png".into(),
                    text: None,
                    path: Some("attachments/gone.png".into()),
                },
            ),
        ];
        let error = fold(&events, &dir).unwrap_err();
        assert!(error.contains("gone.png"), "{error}");
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
                    kind: None,
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

    fn assistant_at(seq: u64, text: &str) -> LogEvent {
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

    fn user_at(seq: u64, text: &str, from: &str) -> LogEvent {
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

    fn compacted_at(seq: u64, through: u64, summary: &str, pins: Vec<u64>) -> LogEvent {
        LogEvent::new(
            seq,
            EventBody::Compacted {
                through_seq: through,
                summary: summary.into(),
                pinned_seqs: pins,
                by: "agent".into(),
                tokens_before: 100,
                tokens_after: 10,
                cache_restart: true,
            },
        )
    }

    fn texts(folded: &Folded) -> Vec<String> {
        folded
            .messages
            .iter()
            .map(|message| message.content.clone().unwrap_or_default())
            .collect()
    }

    /// B1: the summary and the pins render at `through_seq`, before the tail
    /// that was kept verbatim, so the newest event is the last message.
    #[test]
    fn a_compaction_renders_before_the_tail_it_kept() {
        let events = vec![
            started(),
            user_at(2, "the task", "cli"),
            assistant_at(3, "one"),
            assistant_at(4, "two"),
            assistant_at(5, "three"),
            assistant_at(6, "four"),
            // A default-keep compaction: the cut is before the last messages,
            // so its event sits *after* the tail it kept.
            compacted_at(7, 3, "SUMMARY", vec![2]),
            user_at(8, "the newest question", "tui"),
        ];
        let folded = fold(&events, Path::new("")).unwrap();
        let contents = texts(&folded);
        assert_eq!(contents.len(), 6, "{contents:?}");
        assert!(contents[0].contains("SUMMARY"), "{contents:?}");
        assert_eq!(contents[1], "the task", "the pin follows the summary");
        assert_eq!(
            &contents[2..],
            ["two", "three", "four", "the newest question"]
        );
        assert_eq!(
            contents.last().unwrap(),
            "the newest question",
            "the last message is the newest event, never an old pin"
        );
        assert!(!contents.iter().any(|text| text == "one"));
        assert_eq!(
            folded.messages[1].role, "user",
            "the pins are user messages after the summary"
        );
    }

    /// B2: a later compaction whose range reaches over the earlier compaction's
    /// event supersedes it even when its `through_seq` is below that event's
    /// seq: one summary, every pin once.
    #[test]
    fn a_compaction_supersedes_one_whose_range_it_covers() {
        let events = vec![
            started(),
            user_at(2, "the task", "cli"),
            user_at(3, "a note", "tui"),
            assistant_at(4, "one"),
            assistant_at(5, "two"),
            // First compaction: through 4, written at 6, keeping 5 verbatim.
            compacted_at(6, 4, "FIRST", vec![2, 3]),
            assistant_at(7, "three"),
            assistant_at(8, "four"),
            // Second: through 7 — below the first event's seq (6)? No: 7 > 6,
            // but the case that broke was through < the *tail* end, so cover it
            // by keeping the tail after the second compaction.
            compacted_at(9, 7, "SECOND", vec![2, 3]),
            assistant_at(10, "five"),
        ];
        let folded = fold(&events, Path::new("")).unwrap();
        let contents = texts(&folded);
        assert_eq!(contents.len(), 5, "{contents:?}");
        assert!(contents[0].contains("SECOND"), "{contents:?}");
        assert!(!contents.iter().any(|text| text.contains("FIRST")));
        assert_eq!(contents[1], "the task");
        assert_eq!(contents[2], "a note");
        assert_eq!(contents[3], "four");
        assert_eq!(contents[4], "five");
        assert_eq!(
            contents.iter().filter(|text| *text == "the task").count(),
            1,
            "a pin renders once"
        );
    }

    /// The earlier compaction's event can sit *after* a later compaction's
    /// `through_seq` (its kept tail is what pushed it there), and it is still
    /// superseded.
    #[test]
    fn a_compaction_supersedes_one_written_after_its_through_seq() {
        let events = vec![
            started(),
            user_at(2, "the task", "cli"),
            assistant_at(3, "one"),
            compacted_at(4, 2, "FIRST", vec![2]),
            assistant_at(5, "two"),
            assistant_at(6, "three"),
            // through 5 is below the first compaction's seq (4)? It is above,
            // but the first event sits inside the *kept tail* of the second.
            compacted_at(7, 5, "SECOND", vec![2]),
            assistant_at(8, "four"),
        ];
        let folded = fold(&events, Path::new("")).unwrap();
        let contents = texts(&folded);
        assert!(contents[0].contains("SECOND"), "{contents:?}");
        assert!(!contents.iter().any(|text| text.contains("FIRST")));
        assert_eq!(
            contents.iter().filter(|text| *text == "the task").count(),
            1
        );
    }

    /// The transcript of a second compaction includes the first summary: the
    /// range is folded, and the earlier `compacted` event (written later) is
    /// part of it.
    #[test]
    fn a_range_includes_the_summary_of_the_compaction_it_covers() {
        let events = vec![
            started(),
            user_at(2, "the task", "cli"),
            assistant_at(3, "one"),
            compacted_at(4, 2, "FIRST", vec![2]),
            user_at(5, "more", "tui"),
            assistant_at(6, "two"),
        ];
        let range = range_events(&events, 5);
        let folded = fold(&range, Path::new("")).unwrap();
        let contents = texts(&folded);
        assert!(
            contents.iter().any(|text| text.contains("FIRST")),
            "{contents:?}"
        );
        assert!(
            contents.iter().any(|text| text == "the task"),
            "{contents:?}"
        );
        assert!(contents.iter().any(|text| text == "more"), "{contents:?}");
        assert!(
            !contents.iter().any(|text| text == "two"),
            "the tail is outside the range: {contents:?}"
        );
    }

    /// `bg_settled` is a record of what happened, not a message: the model
    /// reads the `user` event that follows it (from `job`), so a job's
    /// completion renders exactly once — the same way live and on resume, which
    /// is what makes a resume's folded `bg_settled` visible at all.
    #[test]
    fn a_settled_job_renders_as_the_message_that_follows_it() {
        let dir = temp_dir("bg");
        let events = vec![
            started(),
            user_at(2, "start the job", "cli"),
            assistant_at(3, "on it"),
            LogEvent::new(
                4,
                EventBody::BgStarted {
                    bg_id: "bg_1".into(),
                    call_id: Some("call_1".into()),
                    cmd: "sleep 60".into(),
                    pid: Some(7),
                    out_path: "bg/bg_1.out".into(),
                },
            ),
            LogEvent::new(
                5,
                EventBody::BgSettled {
                    bg_id: "bg_1".into(),
                    call_id: Some("call_1".into()),
                    cmd: "sleep 60".into(),
                    exit_code: Some(0),
                    state: "exited".into(),
                    out_path: "bg/bg_1.out".into(),
                },
            ),
            user_at(6, "[bg job bg_1 exited 0] ...", "job"),
        ];
        let folded = fold(&events, &dir).unwrap();
        let contents = texts(&folded);
        assert_eq!(
            contents,
            vec!["start the job", "on it", "[bg job bg_1 exited 0] ..."]
        );
        assert_eq!(
            folded.messages.last().unwrap().role,
            "user",
            "a request may not end on the assistant's message here"
        );
        assert_eq!(
            contents
                .iter()
                .filter(|text| text.contains("bg job"))
                .count(),
            1,
            "the job is reported once, not once per event"
        );
        // A `bg_settled` with no message (a session that died before writing
        // one) adds nothing: the resume writes the message, not the fold.
        let only_events: Vec<LogEvent> = events.iter().take(5).cloned().collect();
        let folded = fold(&only_events, &dir).unwrap();
        assert_eq!(texts(&folded), vec!["start the job", "on it"]);
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
