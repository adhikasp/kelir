//! Continuing a session from its log.
//!
//! `resume` never replays a tool call. A call the log shows as requested (an
//! assistant message's `tool_calls`) or started (`tool_started`) but never
//! settled gets one synthetic `tool_settled` whose result tells the model the
//! outcome is unknown, so it can check rather than repeat a `rm -rf`.
//!
//! The same code answers two questions before the log is touched: was a turn in
//! flight when the process died, and what damage does the log carry (a torn
//! last line from a crash mid-write)?

use crate::event::{EventBody, LogEvent, LogRead};

/// What the model is told about a call that was started but never settled.
///
/// The second sentence is the point: a naive replay re-runs the command.
pub const UNKNOWN_RESULT: &str = "unknown outcome: kelir was interrupted while this call was \
running; it may or may not have completed. Check before retrying.";

/// A call the model asked for that never produced a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsettledCall {
    pub call_id: String,
    pub name: String,
    /// A `tool_started` was written, so the process may have run.
    pub started: bool,
}

/// The calls left without a result, in the order the model asked for them.
///
/// Ids are paired by occurrence, not by value: some providers send `call_0`
/// again every step, and a settled `call_0` must not hide a later one.
pub fn unsettled(events: &[LogEvent]) -> Vec<UnsettledCall> {
    let mut open: Vec<UnsettledCall> = Vec::new();
    for event in events {
        match &event.body {
            EventBody::Assistant { tool_calls, .. } => {
                for call in tool_calls {
                    open.push(UnsettledCall {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        started: false,
                    });
                }
            }
            EventBody::ToolStarted { call_id, name, .. } => {
                match open
                    .iter_mut()
                    .find(|call| call.call_id == *call_id && !call.started)
                {
                    Some(call) => call.started = true,
                    None => open.push(UnsettledCall {
                        call_id: call_id.clone(),
                        name: name.clone(),
                        started: true,
                    }),
                }
            }
            EventBody::ToolSettled { call_id, .. } => {
                if let Some(index) = open.iter().position(|call| call.call_id == *call_id) {
                    open.remove(index);
                }
            }
            _ => {}
        }
    }
    open
}

/// Was a turn running when the log stopped? A turn with no `turn_ended` after
/// its last step is continued on resume; a cleanly ended one is not.
pub fn turn_in_flight(events: &[LogEvent]) -> bool {
    let last_end = events
        .iter()
        .rposition(|event| matches!(event.body, EventBody::TurnEnded { .. }));
    events
        .iter()
        .enumerate()
        .skip(last_end.map(|index| index + 1).unwrap_or(0))
        .any(|(_, event)| {
            matches!(
                event.body,
                EventBody::User { .. }
                    | EventBody::StepStarted { .. }
                    | EventBody::Assistant { .. }
                    | EventBody::Usage { .. }
                    | EventBody::ToolStarted { .. }
                    | EventBody::ToolSettled { .. }
            )
        })
}

/// Does a resume have a turn to continue?
///
/// Either a turn was running when the log stopped (no `turn_ended` after its
/// last step), the last turn ended `stopped` or `error` (SPEC's `resume`
/// continues a stopped or crashed session, and a provider outage is the
/// commonest reason to try again — M3 review S1), or the process never closed
/// the log with `session_ended` — a kill between two turns is still an
/// interrupted session. A log whose last turn ended `done` and was closed
/// continues only when the caller passes a new task.
pub fn interrupted(events: &[LogEvent]) -> bool {
    if turn_in_flight(events) {
        return true;
    }
    if matches!(
        last_turn_outcome(events).as_deref(),
        Some("stopped") | Some("error") | Some("paused")
    ) {
        return true;
    }
    !events
        .iter()
        .any(|event| matches!(event.body, EventBody::SessionEnded { .. }))
}

/// The outcome of the last finished turn, if the log has one.
pub fn last_turn_outcome(events: &[LogEvent]) -> Option<String> {
    events.iter().rev().find_map(|event| match &event.body {
        EventBody::TurnEnded { outcome, .. } => Some(outcome.clone()),
        _ => None,
    })
}

/// The seq the log continues from (0 for an empty log).
pub fn last_seq(events: &[LogEvent]) -> u64 {
    events.last().map(|event| event.seq).unwrap_or(0)
}

/// The highest step number in the log, so the resumed session keeps counting
/// rather than repeating steps.
pub fn last_step(events: &[LogEvent]) -> u32 {
    events
        .iter()
        .filter_map(|event| match event.body {
            EventBody::StepStarted { step, .. } => Some(step),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

/// How many turns the log has finished, so the resumed session numbers the next
/// one correctly.
pub fn turns_done(events: &[LogEvent]) -> u32 {
    events
        .iter()
        .filter(|event| matches!(event.body, EventBody::TurnEnded { .. }))
        .count() as u32
}

/// One message per piece of log damage, for the `error` events a resume writes.
/// A torn last line and a line that never parsed are skipped, never dropped
/// silently.
pub fn log_damage(read: &LogRead) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(torn) = &read.torn_last {
        out.push(format!(
            "torn last log line ({} bytes) skipped: the log was cut mid-write; the session \
             continues after it",
            torn.len()
        ));
    }
    for line in &read.bad_lines {
        out.push(format!(
            "unreadable log line skipped: {}",
            crate::util::truncate_chars(line.trim(), 200)
        ));
    }
    out
}

/// The result a resume gives an unsettled call, optionally with the partial
/// output the interrupted call had already written.
pub fn unknown_result(partial: Option<(&str, u64, u64)>) -> String {
    match partial {
        Some((out_path, bytes, lines)) if bytes > 0 => format!(
            "{UNKNOWN_RESULT}\npartial output: {bytes} bytes, {lines} lines; full output at \
             {out_path}"
        ),
        _ => UNKNOWN_RESULT.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{LoggedToolCall, Payload};
    use serde_json::json;

    fn assistant(seq: u64, calls: &[(&str, &str)]) -> LogEvent {
        LogEvent::new(
            seq,
            EventBody::Assistant {
                step: 1,
                text: "".into(),
                reasoning: None,
                tool_calls: calls
                    .iter()
                    .map(|(id, name)| LoggedToolCall {
                        id: (*id).into(),
                        name: (*name).into(),
                        args: Payload::value(json!({"command": "ls"})),
                    })
                    .collect(),
                finish_reason: Some("tool_calls".into()),
            },
        )
    }

    fn started(seq: u64, id: &str) -> LogEvent {
        LogEvent::new(
            seq,
            EventBody::ToolStarted {
                call_id: id.into(),
                name: "bash".into(),
                input: Payload::value(json!({"command": "ls"})),
                pid: Some(7),
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

    fn turn_ended(seq: u64) -> LogEvent {
        LogEvent::new(
            seq,
            EventBody::TurnEnded {
                outcome: "done".into(),
                final_text: "done".into(),
            },
        )
    }

    fn step(seq: u64) -> LogEvent {
        LogEvent::new(
            seq,
            EventBody::StepStarted {
                step: 1,
                provider: "p".into(),
                model: "m".into(),
                request_hash: "0".into(),
            },
        )
    }

    #[test]
    fn a_settled_batch_has_nothing_unsettled() {
        let events = vec![
            assistant(1, &[("a", "bash")]),
            started(2, "a"),
            settled(3, "a"),
        ];
        assert!(unsettled(&events).is_empty());
    }

    #[test]
    fn a_started_call_without_a_result_is_unsettled_and_said_to_have_started() {
        let events = vec![assistant(1, &[("a", "bash")]), started(2, "a")];
        assert_eq!(
            unsettled(&events),
            vec![UnsettledCall {
                call_id: "a".into(),
                name: "bash".into(),
                started: true,
            }]
        );
    }

    #[test]
    fn a_called_but_never_started_call_is_also_unsettled() {
        // The crash after `usage`, before `tool_started`: the request must not
        // be left with an assistant tool call and no result.
        let events = vec![assistant(1, &[("a", "bash"), ("b", "edit")])];
        let calls = unsettled(&events);
        assert_eq!(calls.len(), 2);
        assert!(!calls[0].started);
        assert_eq!(calls[1].call_id, "b");
    }

    #[test]
    fn ids_are_paired_by_occurrence_not_by_value() {
        // A provider that sends `call_0` every step: the old settled one must
        // not hide the new one.
        let events = vec![
            assistant(1, &[("call_0", "bash")]),
            started(2, "call_0"),
            settled(3, "call_0"),
            assistant(4, &[("call_0", "bash")]),
            started(5, "call_0"),
        ];
        let calls = unsettled(&events);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].call_id, "call_0");
        assert!(calls[0].started);
    }

    #[test]
    fn a_partial_batch_reports_only_what_is_missing() {
        let events = vec![
            assistant(1, &[("a", "bash"), ("b", "bash")]),
            started(2, "a"),
            settled(3, "a"),
            started(4, "b"),
        ];
        let calls = unsettled(&events);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].call_id, "b");
    }

    #[test]
    fn a_turn_without_turn_ended_is_in_flight() {
        let done = vec![step(1), assistant(2, &[]), turn_ended(3)];
        assert!(!turn_in_flight(&done));
        let crashed = vec![step(1), assistant(2, &[])];
        assert!(turn_in_flight(&crashed));
        let crashed_mid_tool = vec![
            step(1),
            assistant(2, &[("a", "bash")]),
            started(3, "a"),
            settled(4, "a"),
            turn_ended(5),
            step(6),
        ];
        assert!(turn_in_flight(&crashed_mid_tool));
        // Verbs and errors after the end of a turn do not make it unfinished.
        let idle = vec![
            turn_ended(1),
            LogEvent::new(
                2,
                EventBody::Verb {
                    name: "say".into(),
                    args: Payload::value(json!({"text": "hi"})),
                    from: "ctl".into(),
                    boundary: "queued".into(),
                    error: None,
                },
            ),
        ];
        assert!(!turn_in_flight(&idle));
        assert!(!turn_in_flight(&[]));
    }

    #[test]
    fn an_unclosed_log_is_interrupted_even_between_turns() {
        let ended = |reason: &str| {
            LogEvent::new(
                9,
                EventBody::SessionEnded {
                    reason: reason.into(),
                },
            )
        };
        // A kill between two turns: the turn ended, the process did not.
        let killed_between_turns = vec![step(1), assistant(2, &[]), turn_ended(3)];
        assert!(!turn_in_flight(&killed_between_turns));
        assert!(interrupted(&killed_between_turns));
        // A clean exit is not continued without a new task.
        let mut clean = killed_between_turns.clone();
        clean.push(ended("done"));
        assert!(!interrupted(&clean));
        // A crash mid-turn is interrupted whether or not the log was closed.
        let mut crash = vec![step(1), assistant(2, &[])];
        assert!(interrupted(&crash));
        crash.push(ended("error"));
        assert!(interrupted(&crash), "the step after the last turn end");
    }

    #[test]
    fn a_stopped_or_errored_turn_is_continued_even_when_the_log_was_closed() {
        let ended_with = |outcome: &str| {
            let mut events = vec![step(1), assistant(2, &[]), turn_ended(3)];
            if let EventBody::TurnEnded { outcome: slot, .. } = &mut events[2].body {
                *slot = outcome.to_string();
            }
            events
        };
        let mut stopped = ended_with("stopped");
        stopped.push(LogEvent::new(
            4,
            EventBody::SessionEnded {
                reason: "end".into(),
            },
        ));
        assert!(
            interrupted(&stopped),
            "SPEC: resume continues a stopped session (M3 review S1)"
        );
        let mut failed = ended_with("error");
        failed.push(LogEvent::new(
            4,
            EventBody::SessionEnded {
                reason: "error".into(),
            },
        ));
        assert!(
            interrupted(&failed),
            "a provider outage is the commonest reason to resume"
        );
        let mut done = ended_with("done");
        done.push(LogEvent::new(
            4,
            EventBody::SessionEnded {
                reason: "done".into(),
            },
        ));
        assert!(!interrupted(&done), "a finished session waits for a task");
        assert_eq!(last_turn_outcome(&done).as_deref(), Some("done"));
        assert_eq!(last_turn_outcome(&[]), None);
    }

    #[test]
    fn counters_continue_from_the_log() {
        let events = vec![step(1), step(2), turn_ended(3), step(4), turn_ended(5)];
        assert_eq!(last_seq(&events), 5);
        assert_eq!(last_step(&events), 1, "the sample step numbers repeat");
        assert_eq!(turns_done(&events), 2);
        assert_eq!(last_seq(&[]), 0);
    }

    #[test]
    fn log_damage_names_the_torn_line_and_every_bad_one() {
        let torn = "{\"seq\": 2";
        let read = LogRead {
            events: vec![],
            torn_last: Some(torn.to_string()),
            bad_lines: vec!["not json".into()],
        };
        let damage = log_damage(&read);
        assert_eq!(damage.len(), 2);
        assert!(damage[0].contains("torn last log line"), "{}", damage[0]);
        assert!(
            damage[0].contains(&format!("{} bytes", torn.len())),
            "{}",
            damage[0]
        );
        assert!(damage[1].contains("not json"), "{}", damage[1]);
    }

    #[test]
    fn the_unknown_result_says_what_matters() {
        assert_eq!(unknown_result(None), UNKNOWN_RESULT);
        assert!(UNKNOWN_RESULT.contains("unknown outcome"));
        assert!(UNKNOWN_RESULT.contains("may or may not have completed"));
        assert!(UNKNOWN_RESULT.contains("Check before retrying"));
        let partial = unknown_result(Some(("commands/7-1-1/out", 42, 2)));
        assert!(partial.starts_with(UNKNOWN_RESULT));
        assert!(partial.contains("42 bytes, 2 lines"));
        assert!(partial.contains("commands/7-1-1/out"));
        // Zero bytes is the same as no output at all.
        assert_eq!(unknown_result(Some(("out", 0, 0))), UNKNOWN_RESULT);
    }
}
