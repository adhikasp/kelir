//! Compaction: replace a prefix of the conversation with a summary.
//! Three triggers, one mechanism: the model calls the `compact` tool, a human sends the `compact`
//! verb (`/compact [focus]`, `kelir ctl … compact`), or the context passes the auto fraction of
//! the model's window.
//! Whatever the trigger, it applies at the next `before_step` boundary — never between a tool call

use std::collections::BTreeMap;
use std::path::Path;

use crate::log::event::{EventBody, LogEvent};
use crate::provider::chat::ChatMessage;
use crate::provider::{BuiltRequest, Client};

/// The summary prompt, embedded; `[compaction] prompt` in the config (or
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
/// states one. Provider metadata: [`crate::provider::models`].
pub const DEFAULT_WINDOW: u64 = 128_000;
/// Compaction is triggered when the context passes this fraction of the window.
pub const AUTO_FRACTION: f64 = 0.8;
/// Messages kept verbatim after the summary, so the model still sees what it was
/// doing; `[compaction] keep` in the config overrides it.
pub const DEFAULT_KEEP: usize = 4;

/// Compaction is triggered once the context passes this many tokens, whatever the window is — unless
/// the window-based trigger is smaller.
pub const DEFAULT_SOFT_TARGET: u64 = 150_000;

/// The token count at which the auto trigger fires: the window-based threshold (`window * at`) capped
/// by the absolute soft target.
pub fn auto_threshold(window: u64, at: f64, soft_target: u64) -> u64 {
    let by_window = if window == 0 || !(0.0..=1.0).contains(&at) {
        0
    } else {
        (window as f64 * at).ceil() as u64
    };
    match (by_window, soft_target) {
        (0, 0) => 0,
        (0, target) => target,
        (by_window, 0) => by_window,
        (by_window, target) => by_window.min(target),
    }
}

pub fn should_auto(window: u64, at: f64, soft_target: u64, tokens: u64) -> bool {
    let threshold = auto_threshold(window, at, soft_target);
    threshold > 0 && tokens >= threshold
}

/// A token estimate for a request: four bytes per token, rounded up, over the JSON the provider will
/// receive.
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

fn bears_a_message(event: &LogEvent) -> bool {
    matches!(
        &event.body,
        EventBody::User { .. } | EventBody::Source { .. } | EventBody::Assistant { .. }
    ) || matches!(&event.body, EventBody::CallSettled(settle) if settle.kind == "tool")
}

/// A message typed by a human (as opposed to a job wake, a gate note or a
/// source). Those are the ones compaction pins.
pub fn human_typed(from: &str) -> bool {
    matches!(from, "tui" | "ctl" | "cli" | "channel")
}

/// Work out the next compaction, or `None` when there is nothing to summarise.
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
        && matches!(&bearing[cut].body, EventBody::CallSettled(settle) if settle.kind == "tool")
    {
        cut -= 1;
    }
    if cut == 0 {
        return None;
    }
    let mut through_seq = bearing[cut - 1].seq;

    // A compaction never reaches back before one that already stands: the new one has to cover it (its
    // range and therefore its summary), or the fold would keep both summaries and pin the same messages
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
    // With the floor in force the tail can no longer be pulled back, so drop the orphan risk the
    // other way: a tool result right after the cut moves into the range with it.
    while let Some(next) = bearing.iter().find(|event| event.seq > through_seq) {
        if !matches!(&next.body, EventBody::CallSettled(settle) if settle.kind == "tool") {
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

/// A `compact` call the log shows as queued but that was never applied: the process died between the
/// tool's `compaction queued …` result and the boundary that would have summarised.
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
            EventBody::CallSettled(settle) if settle.kind == "tool" && settle.outcome == "ok" => {
                let Some(result) = &settle.result else {
                    continue;
                };
                let Some(focus) = focus_by_call.get(&settle.id) else {
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
    use crate::log::event::{LoggedToolCall, Payload, SystemInfo};
    use serde_json::json;

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
                usage: None,
            },
        )
    }

    fn settled(seq: u64, id: &str) -> LogEvent {
        LogEvent::new(
            seq,
            crate::log::event::call_settle(
                id,
                "tool",
                "ok",
                crate::log::event::CallSettle {
                    exit_code: Some(0),
                    bytes: Some(2),
                    lines: Some(1),
                    out_path: Some("out".into()),
                    spilled: Some(false),
                    result: Some("hi".into()),
                    ..crate::log::event::CallSettle::default()
                },
            ),
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
                usage: None,
            },
        )
    }

    #[test]
    fn a_zero_soft_target_or_window_is_not_a_threshold() {
        assert_eq!(auto_threshold(0, 0.8, 0), 0);
        assert_eq!(auto_threshold(0, 0.8, 1_000), 1_000);
        assert_eq!(auto_threshold(1_000, 0.5, 0), 500);
        // A nonsense fraction falls back to the target rather than to "never".
        assert_eq!(auto_threshold(1_000, 1.5, 400), 400);
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
        // A `compacted` event whose range ends at 4 is already in the log, but the raw count says the next cut
        // is at 2.
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
                    usage: None,
                },
            ),
            LogEvent::new(
                4,
                crate::log::event::call_settle(
                    "call_c",
                    "tool",
                    "ok",
                    crate::log::event::CallSettle {
                        exit_code: Some(0),
                        bytes: Some(10),
                        lines: Some(1),
                        out_path: Some("out".into()),
                        spilled: Some(false),
                        result: Some("compaction queued: …".into()),
                        ..crate::log::event::CallSettle::default()
                    },
                ),
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
                    usage: None,
                },
            ),
            LogEvent::new(
                4,
                crate::log::event::call_settle(
                    "call_c",
                    "tool",
                    "disabled",
                    crate::log::event::CallSettle {
                        exit_code: None,
                        bytes: Some(4),
                        lines: Some(1),
                        out_path: Some("out".into()),
                        spilled: Some(false),
                        result: Some("disabled by user: compact".into()),
                        ..crate::log::event::CallSettle::default()
                    },
                ),
            ),
        ];
        assert_eq!(queued_compaction(&events, std::path::Path::new("")), None);
    }
}
