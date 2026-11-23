//! Reviewer-added adversarial tests for M3 (see docs/reviews/M3-review.md).
//!
//! These try to break durability, resume and compaction rather than mirror the
//! implementation. A failing test here is a real bug; each names its finding.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use crate::support::*;
use serde_json::json;

fn contents(request: &Recorded) -> Vec<String> {
    request
        .messages()
        .iter()
        .map(|message| {
            let text = message["content"].as_str().unwrap_or("").to_string();
            let calls = message["tool_calls"].as_array().map(|calls| {
                calls
                    .iter()
                    .map(|call| call["id"].as_str().unwrap_or("").to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            });
            match calls {
                Some(calls) => format!("{text}<calls {calls}>"),
                None => text,
            }
        })
        .collect()
}

/// R1: with the default `compact_keep` (4) the kept tail must stay *after* the
/// summary and the pins, in conversation order. `fold` renders a `compacted`
/// event where it was written (after the tail), so the request becomes
/// `[system, tail..., summary, task]`: the model's last message is the stale
/// pinned task, with its own recent work above it.
#[tokio::test]
async fn r1_the_kept_tail_stays_after_the_summary_and_the_pins() {
    let bash = |id: &str, n: u32| {
        Reply::Sse(sse(&tool_turn(
            id,
            "bash",
            &json!({"command": format!("printf 'step{n}\\n'")}),
        )))
    };
    let mock = Mock::start(vec![
        bash("call_1", 1),
        bash("call_2", 2),
        bash("call_3", 3),
        Reply::Sse(sse(&tool_turn("call_c", "compact", &json!({})))),
        Reply::Sse(sse(&text_turn("SUMMARY: three steps done", 0))),
        Reply::Sse(sse(&text_turn("finished", 0))),
    ])
    .await;
    // The default compact_keep is in force: no override.
    let sandbox = Sandbox::new("");
    let run = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "THE TASK"], &[])
        .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let requests = mock.requests();
    assert_eq!(requests.len(), 6);
    let after = contents(&requests[5]);
    eprintln!("request after compaction: {after:#?}");
    let summary_at = after
        .iter()
        .position(|text| text.contains("SUMMARY: three steps done"))
        .expect("the summary");
    let task_at = after
        .iter()
        .position(|text| text == "THE TASK")
        .expect("task");
    let last_tool_at = after
        .iter()
        .rposition(|text| text.starts_with("step3"))
        .expect("the kept tail result");
    assert!(
        summary_at < last_tool_at && task_at < last_tool_at,
        "the kept tail is rendered BEFORE the summary and the pinned task: {after:#?}"
    );
    assert_ne!(
        after.last().unwrap(),
        "THE TASK",
        "the stale pinned task is the model's last message: {after:#?}"
    );
}

/// R2: a compaction must not leave a `tool` message without its assistant call
/// (providers reject it), whatever `compact_keep` is.
#[tokio::test]
async fn r2_no_orphan_tool_messages_after_a_default_compaction() {
    let bash = |id: &str, n: u32| {
        Reply::Sse(sse(&tool_turn(
            id,
            "bash",
            &json!({"command": format!("printf 'step{n}\\n'")}),
        )))
    };
    let mock = Mock::start(vec![
        bash("call_1", 1),
        bash("call_2", 2),
        Reply::Sse(sse(&tool_turn("call_c", "compact", &json!({})))),
        Reply::Sse(sse(&text_turn("SUMMARY: x", 0))),
        Reply::Sse(sse(&text_turn("finished", 0))),
    ])
    .await;
    let sandbox = Sandbox::new("");
    let run = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "T"], &[])
        .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let request = mock.requests().pop().unwrap();
    let mut open: Vec<String> = Vec::new();
    for message in request.messages() {
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                open.push(call["id"].as_str().unwrap().to_string());
            }
        }
        if message["role"] == "tool" {
            let id = message["tool_call_id"].as_str().unwrap().to_string();
            assert!(
                open.contains(&id),
                "orphan tool message {id}: {:#?}",
                contents(&request)
            );
        }
    }
}

/// R3: two compactions close together (default `compact_keep`). The second's
/// `through_seq` can fall before the first `compacted` event (which sits after
/// its kept tail), so the first is not superseded: the request then carries
/// both summaries and the pinned task twice.
#[tokio::test]
async fn r3_a_second_close_compaction_does_not_duplicate_pins_or_summaries() {
    let bash = |id: &str| {
        Reply::Sse(sse(&tool_turn(
            id,
            "bash",
            &json!({"command": "printf 'x\n'"}),
        )))
    };
    let mock = Mock::start(vec![
        bash("call_1"),
        Reply::Sse(sse(&tool_turn("call_c1", "compact", &json!({})))),
        Reply::Sse(sse(&text_turn("SUMMARY-ONE", 0))),
        bash("call_2"),
        Reply::Sse(sse(&tool_turn("call_c2", "compact", &json!({})))),
        Reply::Sse(sse(&text_turn("SUMMARY-TWO", 0))),
        Reply::Sse(sse(&text_turn("finished", 0))),
    ])
    .await;
    let sandbox = Sandbox::new("");
    let run = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "THE TASK"], &[])
        .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let requests = mock.requests();
    let last = contents(requests.last().unwrap());
    eprintln!("final request: {last:#?}");
    let tasks = last.iter().filter(|text| *text == "THE TASK").count();
    let one = last
        .iter()
        .filter(|text| text.contains("SUMMARY-ONE"))
        .count();
    let two = last
        .iter()
        .filter(|text| text.contains("SUMMARY-TWO"))
        .count();
    assert_eq!(tasks, 1, "the task is pinned {tasks} times: {last:#?}");
    assert_eq!(two, 1);
    assert_eq!(
        one, 0,
        "the superseded summary is still in the request: {last:#?}"
    );
}
