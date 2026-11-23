//! Reviewer-added adversarial tests for M3 (see docs/reviews/M3-review.md).
//!
//! These try to break durability, resume and compaction rather than mirror the
//! implementation. A failing test here is a real bug; each names its finding.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use std::sync::Arc;
use std::time::{Duration, Instant};

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

fn tool_ids(request: &Recorded) -> Vec<String> {
    request
        .messages()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| message["tool_call_id"].as_str().unwrap_or("").to_string())
        .collect()
}

/// R6: while the summarising request is in flight the session reads no verbs, so a
/// `stop`/`end` waits for the provider (here 4 s; on a real provider it can be
/// minutes). The ack should not be held hostage by a compaction.
#[tokio::test]
async fn r6_stop_is_answered_while_a_compaction_is_summarising() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("first answer", 0))),
        Reply::Gated(gate.clone(), sse(&text_turn("SUMMARY: s", 0))),
        Reply::Sse(sse(&text_turn("second answer", 0))),
    ])
    .await;
    let mut live = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "the task"],
        "compact_keep = 0
",
    )
    .await;
    live.wait_for("turn_ended").await;
    assert_eq!(live.ctl(&["compact"]).await.code, 0);
    assert_eq!(live.ctl(&["say", "next"]).await.code, 0);
    // The summarising request is now held at the gate.
    for _ in 0..100 {
        if mock.request_count() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        mock.request_count() >= 2,
        "the summarising request never started"
    );
    let started = Instant::now();
    let answer = tokio::time::timeout(Duration::from_secs(3), live.ctl(&["stop"])).await;
    let waited = started.elapsed();
    gate.notify_one();
    assert!(
        answer.is_ok(),
        "`stop` was not answered within {waited:?} while the compaction summarised"
    );
    let _ = live.wait_for("turn_ended").await;
    live.end().await;
}

/// R7: a turn cut off by a provider outage continues on `kelir run --resume`. Since
/// STEER-5 an outage past the waiting budget *pauses* the turn (outcome
/// `paused`, exit 0) rather than failing it; a resume (and a later `say`)
/// retries it from the same fold. M3.md: "A stopped or crashed turn continues
/// from the log with the model's next step."
#[tokio::test]
async fn r7_resume_retries_a_turn_that_ended_in_a_provider_error() {
    let mut script = Vec::new();
    for _ in 0..3 {
        script.push(Reply::Http(500, "{\"error\":\"boom\"}".into()));
    }
    script.push(Reply::Sse(sse(&text_turn("recovered", 0))));
    let mock = Mock::start(script).await;
    // The test harness's own short retry config: three attempts, then a pause.
    let sandbox = Sandbox::new("");
    let first = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "the task"], &[])
        .await;
    assert_eq!(first.code, 0, "{}", first.stderr);
    let id = sandbox.only_session();
    let ended = sandbox
        .log(&id)
        .into_iter()
        .find(|event| event["type"] == "turn_ended")
        .expect("a finished turn");
    assert_eq!(ended["outcome"], "paused", "{ended}");
    let before = mock.request_count();
    let second = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "--resume", id.as_str()],
            &[],
        )
        .await;
    assert_eq!(second.code, 0, "stderr: {}", second.stderr);
    let after = mock.request_count();
    assert!(
        after > before,
        "resume of a paused turn made no provider request (no-op); log: {:?}",
        sandbox
            .log(&id)
            .iter()
            .map(|e| e["type"].as_str().unwrap_or("").to_string())
            .collect::<Vec<_>>()
    );
    let resumed_turn = sandbox
        .log(&id)
        .into_iter()
        .rfind(|event| event["type"] == "turn_ended")
        .expect("the resumed turn");
    assert_eq!(resumed_turn["outcome"], "done", "{resumed_turn}");
}

/// R9: two `resume`s started at the same moment on one crashed session. The
/// "already running" check happens before the socket is bound, so both can pass
/// it and both append to the log (forked seq, two `resumed`, the unknown result
/// twice).
#[tokio::test]
async fn r9_two_simultaneous_resumes_do_not_both_write_the_log() {
    let mut script = vec![Reply::Sse(sse(&tool_turn(
        "call_1",
        "bash",
        &json!({"command": "printf 'x\n'"}),
    )))];
    for _ in 0..4 {
        script.push(Reply::SseAfter(
            Duration::from_millis(400),
            sse(&text_turn("finished", 0)),
        ));
    }
    let mock = Mock::start(script).await;
    let sandbox = Sandbox::new("");
    let first = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "the task"],
            &[("KELIR_TEST_CRASH_AT", "usage")],
        )
        .await;
    assert_ne!(first.code, 0);
    let id = sandbox.only_session();
    let args = ["run", "--json", "--resume", id.as_str()];
    let url = mock.base_url();
    let (a, b) = tokio::join!(
        sandbox.kelir(&url, &args, &[]),
        sandbox.kelir(&url, &args, &[]),
    );
    eprintln!("a: {} {}\nb: {} {}", a.code, a.stderr, b.code, b.stderr);
    let log = sandbox.log(&id);
    let seqs: Vec<u64> = log.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    let dense = seqs.windows(2).all(|w| w[1] == w[0] + 1);
    assert!(
        dense,
        "two writers on one log: seqs={seqs:?}, exits {} / {}",
        a.code, b.code
    );
}

/// R10: a human message over 10 KB is spilled to a blob; compaction pins it, and
/// the request after the compaction must still carry it verbatim.
#[tokio::test]
async fn r10_a_spilled_human_message_is_pinned_verbatim() {
    let big = format!("BIGSTART {} BIGEND", "x".repeat(30_000));
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("one", 0))),
        Reply::Sse(sse(&text_turn("two", 0))),
        Reply::Sse(sse(&text_turn("SUMMARY: s", 0))),
        Reply::Sse(sse(&text_turn("three", 0))),
    ])
    .await;
    let mut live = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "the task"],
        "compact_keep = 0\n",
    )
    .await;
    live.wait_for("turn_ended").await;
    assert_eq!(live.ctl(&["say", &big]).await.code, 0);
    live.wait_for("turn_ended").await;
    assert_eq!(live.ctl(&["compact"]).await.code, 0);
    assert_eq!(live.ctl(&["say", "last"]).await.code, 0);
    live.wait_for("turn_ended").await;
    live.end().await;
    let request = mock.requests().pop().unwrap();
    let all = contents(&request);
    assert!(
        all.contains(&big),
        "the spilled human message is not verbatim in the request: {:?}",
        all.iter().map(|t| t.len()).collect::<Vec<_>>()
    );
}

/// R11: the summarising request fails (provider error). The turn must go on, and
/// the auto trigger must not hammer the provider with a summary request on
/// every following step.
#[tokio::test]
async fn r11_a_failing_summary_is_not_retried_on_every_step() {
    let mut script = vec![Reply::Sse(sse(&tool_turn(
        "call_1",
        "bash",
        &json!({"command": "printf 'a\n'"}),
    )))];
    for _ in 0..30 {
        script.push(Reply::Http(500, "{\"error\":\"boom\"}".into()));
    }
    let mock = Mock::start(script).await;
    let sandbox = Sandbox::new("window = 100\ncompact_keep = 0\n[retry]\nmax_attempts = 1\n");
    let run = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "the task"], &[])
        .await;
    eprintln!("exit {} stderr {}", run.code, run.stderr);
    let summaries = mock
        .requests()
        .iter()
        .filter(|request| request.body.get("tools").is_none())
        .count();
    let id = sandbox.only_session();
    let types: Vec<String> = sandbox
        .log(&id)
        .iter()
        .map(|e| e["type"].as_str().unwrap_or("").to_string())
        .collect();
    eprintln!("{types:?}");
    assert!(summaries <= 2, "{summaries} summarising requests were sent");
}
