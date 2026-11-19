//! Compaction: replace a prefix of the conversation with a summary.
//!
//! Three triggers, one mechanism: the model calls the `compact` tool, a human
//! sends the `compact` verb (`/compact [focus]`, `kelir ctl … compact`), or the
//! context passes the auto fraction of the model's window. Whatever the
//! trigger, it applies at the next `before_step` boundary — never between a tool
//! call and its result — and produces one `compacted` event.
//!
//! The summarising request is sealed: a system message (the `compact.md` prompt)
//! and one user message (the transcript, plus the focus). No tools, so the model
//! cannot answer a summary request by calling `bash`.
//!
//! `fold` then renders the range as the summary plus the pinned messages: the
//! first user message (the task) and every message a human typed.

use std::collections::BTreeMap;
use std::path::Path;

use crate::chat::ChatMessage;
use crate::event::{EventBody, LogEvent};
use crate::provider::{BuiltRequest, Client};

/// The summary prompt, embedded; `compact_prompt` in the config (or
/// `compact.md` in the config dir) replaces it.
pub const DEFAULT_COMPACT_PROMPT: &str = "\
You are compacting the conversation of an autonomous coding agent so it can keep \
working in a smaller context.

Write a dense, factual summary of the conversation you are given: the task and \
its constraints, what was decided, what was changed and where, what was verified \
and how, what failed and why, and what is still open. Keep file paths, command \
lines and identifiers exact; do not invent anything. Do not address the agent, do \
not add advice, and do not use headings — plain prose, under 400 words.";

/// The window used when neither the config nor the provider's model list
/// states one. Provider metadata: [`crate::models`].
pub const DEFAULT_WINDOW: u64 = 128_000;
/// Compaction is triggered when the context passes this fraction of the window.
pub const AUTO_FRACTION: f64 = 0.8;
/// Messages kept verbatim after the summary, so the model still sees what it was
/// doing; `compact_keep` in the config overrides it.
pub const DEFAULT_KEEP: usize = 4;

/// Passed the auto threshold? `at` is the fraction of the window (0.8 by
/// default); the comparison is `>=`, so `window * at` exactly triggers.
pub fn should_auto(window: u64, at: f64, tokens: u64) -> bool {
    if window == 0 || !(0.0..=1.0).contains(&at) {
        return false;
    }
    tokens >= (window as f64 * at).ceil() as u64
}

/// A token estimate for a request: four bytes per token, rounded up, over the
/// JSON the provider will receive. Good enough for a trigger and for the
/// `tokens_before`/`tokens_after` numbers, which are labelled as estimates.
pub fn estimate_tokens(messages: &[ChatMessage]) -> u64 {
    let mut bytes = 0usize;
    for message in messages {
        bytes += serde_json::to_vec(message)
            .map(|line| line.len())
            .unwrap_or(0);
    }
    bytes.div_ceil(4) as u64
}

/// What a compaction would do, worked out from the log alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Everything up to and including this seq leaves the context.
    pub through_seq: u64,
    /// The user events `fold` renders verbatim in place of the range, in log
    /// order.
    pub pinned_seqs: Vec<u64>,
}

/// Does this event render as a chat message?
fn bears_a_message(event: &LogEvent) -> bool {
    matches!(
        event.body,
        EventBody::User { .. }
            | EventBody::Source { .. }
            | EventBody::Assistant { .. }
            | EventBody::ToolSettled { .. }
    )
}

/// A message typed by a human (as opposed to a job wake, a gate note or a
/// source). Those are the ones compaction pins.
pub fn human_typed(from: &str) -> bool {
    matches!(from, "tui" | "ctl" | "cli" | "channel")
}

/// Work out the next compaction, or `None` when there is nothing to summarise.
///
/// The last `keep` messages stay verbatim. The cut never leaves the tail
/// starting on a tool result: a provider rejects a `tool` message whose
/// assistant call was summarised away, so the cut moves back to that call.
pub fn plan(events: &[LogEvent], keep: usize) -> Option<Plan> {
    let bearing: Vec<&LogEvent> = events
        .iter()
        .filter(|event| bears_a_message(event))
        .collect();
    if bearing.is_empty() {
        return None;
    }
    let mut cut = bearing.len().saturating_sub(keep);
    while cut > 0
        && cut < bearing.len()
        && matches!(bearing[cut].body, EventBody::ToolSettled { .. })
    {
        cut -= 1;
    }
    if cut == 0 {
        return None;
    }
    let mut through_seq = bearing[cut - 1].seq;

    // A compaction never reaches back before one that already stands: the new
    // one has to cover it (its range and therefore its summary), or the fold
    // would keep both summaries and pin the same messages twice (B2).
    let floor = events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::Compacted { through_seq, .. } => Some(*through_seq),
            _ => None,
        })
        .max();
    if let Some(floor) = floor
        && through_seq < floor
    {
        through_seq = floor;
    }
    // With the floor in force the tail can no longer be pulled back, so drop
    // the orphan risk the other way: a tool result right after the cut moves
    // into the range with it.
    while let Some(next) = bearing.iter().find(|event| event.seq > through_seq) {
        if !matches!(next.body, EventBody::ToolSettled { .. }) {
            break;
        }
        through_seq = next.seq;
    }

    let first_user = events
        .iter()
        .find(|event| matches!(event.body, EventBody::User { .. }))
        .map(|event| event.seq);
    let mut pinned_seqs = Vec::new();
    for event in events {
        if event.seq > through_seq {
            break;
        }
        if let EventBody::User { from, .. } = &event.body
            && (Some(event.seq) == first_user || human_typed(from))
        {
            pinned_seqs.push(event.seq);
        }
    }
    Some(Plan {
        through_seq,
        pinned_seqs,
    })
}

/// A `compact` call the log shows as queued but that was never applied: the
/// process died between the tool's `compaction queued …` result and the
/// boundary that would have summarised. A resume re-queues it (N2), so the
/// model's request is not silently lost.
///
/// `None` means nothing is outstanding; `Some(focus)` is a queued compaction,
/// with the focus the call carried when it had one.
pub fn queued_compaction(events: &[LogEvent], session_dir: &Path) -> Option<Option<String>> {
    let mut focus_by_call: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut pending: Option<Option<String>> = None;
    for event in events {
        match &event.body {
            EventBody::Assistant { tool_calls, .. } => {
                for call in tool_calls.iter().filter(|call| call.name == "compact") {
                    let focus = call
                        .args
                        .resolve(session_dir)
                        .ok()
                        .and_then(|args| {
                            args.get("focus")
                                .and_then(|value| value.as_str())
                                .map(str::to_string)
                        })
                        .filter(|focus| !focus.trim().is_empty());
                    focus_by_call.insert(call.id.clone(), focus);
                }
            }
            EventBody::ToolSettled {
                call_id,
                outcome,
                result,
                ..
            } if outcome == "ok" => {
                let Some(focus) = focus_by_call.get(call_id) else {
                    continue;
                };
                let queued = result
                    .resolve_text(session_dir)
                    .map(|text| text.contains("compaction queued"))
                    .unwrap_or(false);
                if queued {
                    pending = Some(focus.clone());
                }
            }
            // Any compaction after the queue's own event settles it.
            EventBody::Compacted { .. } => pending = None,
            _ => {}
        }
    }
    pending
}

/// The conversation as text, for the summarising request.
pub fn render_transcript(messages: &[ChatMessage]) -> String {
    let mut out = String::new();
    for message in messages {
        out.push_str("### ");
        out.push_str(&message.role);
        out.push('\n');
        if let Some(content) = &message.content {
            out.push_str(content);
            out.push('\n');
        }
        if let Some(calls) = &message.tool_calls {
            for call in calls {
                out.push_str("→ call ");
                out.push_str(&call.id);
                out.push(' ');
                out.push_str(&call.function.name);
                out.push(' ');
                out.push_str(&call.function.arguments);
                out.push('\n');
            }
        }
        out.push('\n');
    }
    out
}

/// The summarising request. Sealed on purpose: the prompt and one user message,
/// no tools, so the summary cannot be a tool call.
pub fn summarise_request(
    client: &Client,
    model: &str,
    prompt: &str,
    transcript: &str,
    focus: Option<&str>,
) -> BuiltRequest {
    let mut user = String::new();
    if let Some(focus) = focus.map(str::trim).filter(|focus| !focus.is_empty()) {
        user.push_str("Focus for this summary: ");
        user.push_str(focus);
        user.push_str("\n\n");
    }
    user.push_str("Conversation to summarise:\n\n");
    user.push_str(transcript);
    let messages = vec![ChatMessage::system(prompt), ChatMessage::user(&user)];
    client.build(model, &messages, &[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ProviderConfig, ProviderKind};
    use crate::event::{LoggedToolCall, Payload, SystemInfo};
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

    fn user(seq: u64, text: &str, from: &str) -> LogEvent {
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

    fn assistant_call(seq: u64, id: &str) -> LogEvent {
        LogEvent::new(
            seq,
            EventBody::Assistant {
                step: 1,
                text: "".into(),
                reasoning: None,
                tool_calls: vec![LoggedToolCall {
                    id: id.into(),
                    name: "bash".into(),
                    args: Payload::value(json!({"command": "ls"})),
                }],
                finish_reason: Some("tool_calls".into()),
            },
        )
    }

    fn settled(seq: u64, id: &str) -> LogEvent {
        LogEvent::new(
            seq,
            EventBody::ToolSettled {
                call_id: id.into(),
                outcome: "ok".into(),
                exit_code: Some(0),
                bytes: 2,
                lines: 1,
                out_path: "out".into(),
                spilled: false,
                result: "hi".into(),
            },
        )
    }

    fn assistant_text(seq: u64, text: &str) -> LogEvent {
        LogEvent::new(
            seq,
            EventBody::Assistant {
                step: 1,
                text: text.into(),
                reasoning: None,
                tool_calls: vec![],
                finish_reason: Some("stop".into()),
            },
        )
    }

    #[test]
    fn the_auto_trigger_is_eighty_percent_of_the_window() {
        assert!(!should_auto(100, 0.8, 79));
        assert!(should_auto(100, 0.8, 80));
        assert!(should_auto(100, 0.8, 81));
        assert!(should_auto(128_000, 0.8, 102_400));
        assert!(!should_auto(128_000, 0.8, 102_399));
        // A half trigger is configurable.
        assert!(should_auto(1_000, 0.5, 500));
        assert!(!should_auto(1_000, 0.5, 499));
        // Nonsense never triggers.
        assert!(!should_auto(0, 0.8, 10));
        assert!(!should_auto(100, 1.5, 200));
    }

    #[test]
    fn the_estimate_is_four_bytes_a_token() {
        let messages = vec![ChatMessage::system("abcd"), ChatMessage::user("12345678")];
        let estimate = estimate_tokens(&messages);
        let bytes: usize = messages
            .iter()
            .map(|message| serde_json::to_vec(message).unwrap().len())
            .sum();
        assert_eq!(estimate, bytes.div_ceil(4) as u64);
        assert!(estimate > 4);
    }

    #[test]
    fn a_plan_keeps_the_tail_and_pins_the_task_and_human_messages() {
        let events = vec![
            started(),
            user(2, "the task", "cli"),
            user(3, "a hint from a job", "job"),
            assistant_call(4, "a"),
            settled(5, "a"),
            user(6, "steering", "tui"),
            assistant_text(7, "answer"),
        ];
        let plan = plan(&events, 1).expect("a plan");
        assert_eq!(plan.through_seq, 6, "everything but the last message goes");
        assert_eq!(
            plan.pinned_seqs,
            vec![2, 6],
            "the first user message and the human one; not the job hint"
        );
    }

    #[test]
    fn nothing_is_compacted_when_the_log_is_shorter_than_the_tail() {
        let events = vec![
            started(),
            user(2, "the task", "cli"),
            assistant_text(3, "hi"),
        ];
        assert_eq!(plan(&events, 4), None);
        assert_eq!(
            plan(&events, 1),
            Some(Plan {
                through_seq: 2,
                pinned_seqs: vec![2],
            })
        );
    }

    #[test]
    fn the_cut_never_starts_the_tail_on_a_tool_result() {
        let events = vec![
            started(),
            user(2, "the task", "cli"),
            assistant_call(3, "a"),
            settled(4, "a"),
        ];
        // keep = 1 would put the cut before the tool result; the assistant call
        // moves back with it, so the tail starts at the assistant instead.
        let plan = plan(&events, 1).expect("a plan");
        assert_eq!(plan.through_seq, 2);
        assert_eq!(plan.pinned_seqs, vec![2]);
    }

    #[test]
    fn the_first_user_message_is_pinned_whatever_its_sender() {
        let events = vec![
            started(),
            user(2, "a job wake", "job"),
            assistant_text(3, "worked on it"),
        ];
        let plan = plan(&events, 1).expect("a plan");
        assert_eq!(plan.through_seq, 2);
        assert_eq!(plan.pinned_seqs, vec![2]);
    }

    #[test]
    fn a_plan_never_reaches_back_before_a_compaction_that_stands() {
        // A `compacted` event whose range ends at 4 is already in the log, but
        // the raw count says the next cut is at 2. The new range must cover the
        // old one, so its summary and pins are replaced, not repeated (B2).
        let events = vec![
            started(),
            user(2, "the task", "cli"),
            assistant_call(3, "a"),
            settled(4, "a"),
            LogEvent::new(
                5,
                EventBody::Compacted {
                    through_seq: 4,
                    summary: "FIRST".into(),
                    pinned_seqs: vec![2],
                    by: "agent".into(),
                    tokens_before: 10,
                    tokens_after: 5,
                    cache_restart: true,
                },
            ),
            assistant_text(6, "after"),
        ];
        let plan = plan(&events, 1).expect("a plan");
        assert_eq!(plan.through_seq, 4, "the old range is covered");
        assert_eq!(plan.pinned_seqs, vec![2]);
    }

    #[test]
    fn a_queued_compaction_is_found_until_one_is_applied() {
        let dir = std::path::Path::new("");
        let mut events = vec![
            started(),
            user(2, "the task", "cli"),
            LogEvent::new(
                3,
                EventBody::Assistant {
                    step: 1,
                    text: "".into(),
                    reasoning: None,
                    tool_calls: vec![LoggedToolCall {
                        id: "call_c".into(),
                        name: "compact".into(),
                        args: Payload::value(json!({"focus": "the api shape"})),
                    }],
                    finish_reason: Some("tool_calls".into()),
                },
            ),
            LogEvent::new(
                4,
                EventBody::ToolSettled {
                    call_id: "call_c".into(),
                    outcome: "ok".into(),
                    exit_code: Some(0),
                    bytes: 10,
                    lines: 1,
                    out_path: "out".into(),
                    spilled: false,
                    result: "compaction queued: …".into(),
                },
            ),
        ];
        assert_eq!(
            queued_compaction(&events, dir),
            Some(Some("the api shape".to_string())),
            "a settle with no compaction after it is still queued"
        );
        events.push(LogEvent::new(
            5,
            EventBody::Compacted {
                through_seq: 4,
                summary: "SUMMARY".into(),
                pinned_seqs: vec![2],
                by: "agent".into(),
                tokens_before: 10,
                tokens_after: 5,
                cache_restart: true,
            },
        ));
        assert_eq!(queued_compaction(&events, dir), None, "already applied");
    }

    #[test]
    fn a_compaction_that_failed_is_not_requeued() {
        let events = vec![
            started(),
            user(2, "the task", "cli"),
            LogEvent::new(
                3,
                EventBody::Assistant {
                    step: 1,
                    text: "".into(),
                    reasoning: None,
                    tool_calls: vec![LoggedToolCall {
                        id: "call_c".into(),
                        name: "compact".into(),
                        args: Payload::value(json!({})),
                    }],
                    finish_reason: Some("tool_calls".into()),
                },
            ),
            LogEvent::new(
                4,
                EventBody::ToolSettled {
                    call_id: "call_c".into(),
                    outcome: "disabled".into(),
                    exit_code: None,
                    bytes: 4,
                    lines: 1,
                    out_path: "out".into(),
                    spilled: false,
                    result: "disabled by user: compact".into(),
                },
            ),
        ];
        assert_eq!(queued_compaction(&events, std::path::Path::new("")), None);
    }

    #[test]
    fn the_transcript_names_roles_calls_and_outputs() {
        let messages = vec![
            ChatMessage::user("the task"),
            ChatMessage::user("the task"),
            ChatMessage::assistant(
                "looking",
                &[crate::chat::ToolCall {
                    id: "call_1".into(),
                    name: "bash".into(),
                    args: json!({"command": "ls -la"}),
                }],
            ),
            ChatMessage::tool("call_1", "a.txt"),
        ];
        let text = render_transcript(&messages);
        assert!(text.contains("### user\nthe task"), "{text}");
        assert!(text.contains("### assistant\nlooking"), "{text}");
        assert!(text.contains("→ call call_1 bash"), "{text}");
        assert!(text.contains("ls -la"), "{text}");
        assert!(text.contains("### tool\na.txt"), "{text}");
    }

    fn client() -> Client {
        let cfg = ProviderConfig {
            kind: ProviderKind::OpenRouter,
            base_url: "https://example.invalid/v1".into(),
            key_env: None,
            key: Some("k".into()),
            extra_headers: BTreeMap::new(),
            reasoning_field: None,
            session_header: None,
            max_concurrent_requests: None,
        };
        Client::new(cfg, "ses_1", None).unwrap()
    }

    #[test]
    fn the_summarising_request_is_sealed() {
        let request = summarise_request(
            &client(),
            "test/model",
            "PROMPT",
            "### user\nthe task\n",
            Some("the failing test"),
        );
        // No tools: the summary cannot be a tool call.
        assert!(request.body.get("tools").is_none());
        let messages = request.body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2, "system + transcript");
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "PROMPT");
        assert_eq!(messages[1]["role"], "user");
        let user = messages[1]["content"].as_str().unwrap();
        assert!(user.contains("Focus for this summary: the failing test"));
        assert!(user.contains("Conversation to summarise:"));
        assert!(user.contains("the task"));
        // The same inputs build the same body: a sealed request is cache-able.
        let again = summarise_request(
            &client(),
            "test/model",
            "PROMPT",
            "### user\nthe task\n",
            Some("the failing test"),
        );
        assert_eq!(request.body, again.body);
    }
}
