//! `fold(events.jsonl)`: the one code path that turns the log into context.
//! Live and on resume the request is built from this, never from memory.
//! Spilled payloads are read back here, so a request built from blobs is byte-identical to
//! one built from inline values (the cache prefix cannot change because the log got large).

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use crate::log::event::{EventBody, LogEvent};
use crate::provider::chat::{ChatMessage, ToolCall};

#[derive(Debug, Clone, Default)]
pub struct Folded {
    /// The frozen system text from `session_started`.
    pub system: String,
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

#[optimize(speed)]
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

    // A compaction renders at `through_seq` — where its range ends — not where its event was written.
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

/// The events a compaction of `through_seq` is summarised from: everything in the range, plus every
/// `compacted` event whose own range is inside it.
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
                // An image queued by `kelir view`: a user message holding one image part, at the tail.
                let rel = path.clone().unwrap_or_else(|| name.clone());
                let file = session_dir.join(&rel);
                let bytes = std::fs::read(&file).map_err(|error| {
                    format!(
                        "[source: attachment {rel}] cannot read {}: {error}",
                        crate::util::path_string(&file)
                    )
                })?;
                let mime = crate::features::attachments::mime_of(&file);
                folded.messages.push(ChatMessage::user_with_images(
                    "",
                    vec![crate::provider::chat::ImagePart {
                        mime: mime.to_string(),
                        data_url: crate::features::attachments::data_url(mime, &bytes),
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
        EventBody::CallSettled(settle) if settle.kind == "tool" && settle.result.is_some() => {
            folded.messages.push(ChatMessage::tool(
                &settle.id,
                &settle
                    .result
                    .as_ref()
                    .expect("checked above")
                    .resolve_text(session_dir)?,
            ));
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::event::{
        Blobs, EventBody, LogEvent, LoggedToolCall, Payload, SystemInfo, spill_payloads,
    };
    use serde_json::json;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kelir-fold-{}-{name}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn started() -> LogEvent {
        LogEvent::new(
            1,
            EventBody::SessionStarted {
                id: "ses_1".into(),
                format: 1,
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
                key_origin: None,
            },
        )
    }

    fn user(text: &str) -> LogEvent {
        user_at(2, text, "cli")
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
                    usage: None,
                },
            ),
            LogEvent::new(
                4,
                crate::log::event::call_settle(
                    "c1",
                    "tool",
                    "ok",
                    crate::log::event::CallSettle {
                        exit_code: Some(0),
                        bytes: Some(3),
                        lines: Some(1),
                        out_path: Some("out".into()),
                        spilled: Some(false),
                        result: Some("hi\n".into()),
                        ..crate::log::event::CallSettle::default()
                    },
                ),
            ),
            LogEvent::new(
                5,
                EventBody::Assistant {
                    step: 2,
                    text: "done".into(),
                    reasoning: None,
                    tool_calls: vec![],
                    finish_reason: Some("stop".into()),
                    usage: None,
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
                    usage: None,
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
    /// file at request time: the log never holds the bytes. A missing file is
    /// an error, never a silent skip.
    #[test]
    fn attachment_sources_render_at_the_tail_or_fail() {
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
        let events_for = |name: &str| {
            vec![
                started(),
                user("what is in this?"),
                LogEvent::new(
                    3,
                    EventBody::Source {
                        kind: "attachment".into(),
                        name: name.into(),
                        text: None,
                        path: Some(name.into()),
                    },
                ),
            ]
        };
        let folded = fold(&events_for("attachments/1-x.png"), &dir).unwrap();
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

        let error = fold(&events_for("attachments/gone.png"), &dir).unwrap_err();
        assert!(error.contains("gone.png"), "{error}");
        assert!(error.contains("cannot read"), "{error}");
    }

    #[test]
    fn usage_and_meta_events_do_not_render() {
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
                    usage: Some(crate::log::event::test_usage(1, 1, 1, None)),
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
            usage: None,
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
    fn a_compaction_replaces_its_range_and_pins_verbatim_in_order() {
        let events = vec![
            started(),
            user_at(2, "the task", "cli"),
            user_at(3, "a hint from a job", "job"),
            assistant_at(4, "step one"),
            user_at(5, "human steering", "ctl"),
            assistant_at(6, "step two"),
            compacted_at(7, 6, "SUMMARY", vec![5, 2]),
            user_at(8, "after the compaction", "tui"),
        ];
        let folded = fold(&events, Path::new("")).unwrap();
        assert_eq!(folded.system, "SYS", "the system prefix survives");
        let contents = texts(&folded);
        let contents: Vec<&str> = contents
            .iter()
            .map(|text| text.trim_start_matches("[kelir compacted the earlier conversation]\n\n"))
            .collect();
        assert_eq!(
            contents,
            [
                "SUMMARY",
                "the task",
                "human steering",
                "after the compaction"
            ],
            "pins keep their log order, verbatim"
        );
        assert!(!contents.iter().any(|text| text.contains("step one")));
        assert!(!contents.iter().any(|text| text.contains("step two")));
        assert!(
            !contents
                .iter()
                .any(|text| text.contains("a hint from a job"))
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
                usage: None,
            },
        )
    }

    // A user at an arbitrary seq/from; `user` above is the common cli-at-2 case.
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

    /// One compaction keeps its pins in log order before the verbatim tail;
    /// a later one supersedes the earlier summary even when its range stops
    /// short of the earlier event, and renders the surviving pin only once.
    #[test]
    fn compaction_rendering_and_supersession() {
        // Each case is (events after `started`, expected contents).
        let cases: [(&str, Vec<LogEvent>, &[&str]); 4] = [
            (
                "one compaction before its kept tail",
                vec![
                    user_at(2, "the task", "cli"),
                    assistant_at(3, "one"),
                    assistant_at(4, "two"),
                    compacted_at(5, 3, "SUMMARY", vec![2]),
                    user_at(6, "the newest question", "tui"),
                ],
                &["SUMMARY", "the task", "two", "the newest question"],
            ),
            (
                "second compaction supersedes the first",
                vec![
                    user_at(2, "the task", "cli"),
                    user_at(3, "a note", "tui"),
                    assistant_at(4, "one"),
                    compacted_at(5, 4, "FIRST", vec![2, 3]),
                    assistant_at(6, "two"),
                    compacted_at(7, 6, "SECOND", vec![2, 3]),
                    assistant_at(8, "three"),
                ],
                &["SECOND", "the task", "a note", "three"],
            ),
            (
                "earlier event inside the later kept tail",
                vec![
                    user_at(2, "the task", "cli"),
                    assistant_at(3, "one"),
                    compacted_at(4, 2, "FIRST", vec![2]),
                    assistant_at(5, "two"),
                    compacted_at(6, 5, "SECOND", vec![2]),
                    assistant_at(7, "three"),
                ],
                &["SECOND", "the task", "three"],
            ),
            (
                "second compaction with pins from both ranges",
                vec![
                    user_at(2, "the task", "cli"),
                    assistant_at(3, "one"),
                    compacted_at(4, 3, "FIRST", vec![2]),
                    user_at(5, "more", "tui"),
                    assistant_at(6, "two"),
                    compacted_at(7, 6, "SECOND", vec![2, 5]),
                    assistant_at(8, "three"),
                ],
                &["SECOND", "the task", "more", "three"],
            ),
        ];
        for (name, mut tail, expected) in cases {
            let mut events = vec![started()];
            events.append(&mut tail);
            let folded = fold(&events, Path::new("")).unwrap();
            assert_eq!(
                texts(&folded)
                    .iter()
                    .map(|s| s.trim_start_matches("[kelir compacted the earlier conversation]\n\n"))
                    .collect::<Vec<_>>(),
                expected,
                "{name}"
            );
            assert!(!texts(&folded).iter().any(|text| text.contains("FIRST")));
            assert_eq!(
                texts(&folded)
                    .iter()
                    .filter(|text| *text == "the task")
                    .count(),
                1,
                "a pin renders once: {name}"
            );
        }
    }

    /// The transcript of a second compaction includes the first summary: the range is folded, and
    /// the earlier `compacted` event (written later) is part of it.
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

    /// follows it (from `job`), so a job's completion renders exactly once — the same way live and on
    #[test]
    fn a_settled_job_renders_as_the_message_that_follows_it() {
        let dir = temp_dir("bg");
        let events = vec![
            started(),
            user_at(2, "start the job", "cli"),
            assistant_at(3, "on it"),
            LogEvent::new(
                4,
                crate::log::event::call_start(
                    "bg_1",
                    "bg",
                    crate::log::event::CallInfo::named("bg", "sleep 60".into()),
                ),
            ),
            LogEvent::new(
                5,
                crate::log::event::call_settle(
                    "bg_1",
                    "bg",
                    "exited",
                    crate::log::event::CallSettle {
                        exit_code: Some(0),
                        state: Some("exited".into()),
                        ..crate::log::event::CallSettle::default()
                    },
                ),
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
                        args: Payload::Blob(crate::log::event::BlobRef {
                            spilled: true,
                            path: "blobs/2-tool_calls.0.args".into(),
                            bytes: 10,
                            preview: "…".into(),
                        }),
                    }],
                    finish_reason: None,
                    usage: None,
                },
            ),
        ];
        let error = fold(&log, &dir).unwrap_err();
        assert!(error.contains("cannot read spilled payload"), "{error}");
    }
}
