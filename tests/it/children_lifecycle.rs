//! children lifecycle tests: an_idle_keep_session_wakes_on_, ctl_brief_prints_the_answer_an, only_the_root_writes_the_herdr.
#![allow(clippy::pedantic, clippy::nursery)]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::children_tree::*;
use crate::support::*;
use crate::support::{CEILING, LiveSession, Mock, Reply, TreeGuard};
use serde_json::{Value, json};

#[tokio::test]
async fn an_idle_keep_session_wakes_on_a_background_child() {
    let mock = Mock::start_routed(vec![], |request| {
        if last_text(request).starts_with("[child ses_") {
            return Some(say("woke up"));
        }
        let task = task_of(request);
        let results = tool_results(request).len();
        Some(match (task.as_str(), results) {
            ("ROOT", 0) => bash("call_bg", &child_cmd("--background", "QUICK look")),
            ("ROOT", _) => say("started"),
            (quick, _) if quick.starts_with("QUICK") => {
                Reply::SseAfter(Duration::from_secs(2), sse(&text_turn("quick answer", 0)))
            }
            _ => return None,
        })
    })
    .await;
    let mut root = LiveSession::start(&mock, &["run", "--keep", "--json", "ROOT"], "").await;
    let started = Instant::now();
    loop {
        let log = root.log();
        let assistants: Vec<String> = log
            .iter()
            .filter(|event| event["type"] == "assistant")
            .map(|event| event["text"].as_str().unwrap_or("").to_string())
            .collect();
        if assistants.iter().any(|text| text == "woke up") {
            // The turn ended on "started" first: the session was idle when the
            // child settled, and the settlement started a new turn.
            assert!(
                assistants.iter().any(|text| text == "started"),
                "{assistants:?}"
            );
            let wake = log
                .iter()
                .find(|event| event["type"] == "user" && event["from"] == "child")
                .expect("the wake message");
            assert!(wake["text"].as_str().unwrap().contains("quick answer"));
            break;
        }
        assert!(
            started.elapsed() < CEILING,
            "the idle session never woke: {assistants:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    root.kill().await;
}

#[tokio::test]
async fn ctl_brief_prints_the_answer_and_leaves_the_history_alone() {
    let mock = Mock::start_routed(vec![], |request| {
        Some(if is_brief(request) {
            say("ctl brief answer")
        } else {
            say("hello")
        })
    })
    .await;
    let mut root = LiveSession::start(&mock, &["run", "--keep", "--json", "ROOT"], "").await;
    // Wait for the first turn to end so the snapshot has an answer in it.
    let started = Instant::now();
    while !root.log().iter().any(|event| event["type"] == "turn_ended") {
        assert!(started.elapsed() < CEILING);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let answer = root.ctl(&["brief", "BRIEFQ what did you say?"]).await;
    assert_eq!(answer.code, 0, "{} {}", answer.stdout, answer.stderr);
    assert!(
        answer.stdout.contains("ctl brief answer"),
        "{}",
        answer.stdout
    );
    let log = root.log();
    assert_eq!(
        of_type(&log, "user").len(),
        1,
        "the question is not in the history"
    );
    assert_eq!(
        calls_of(&log, "call_started", "child")[0]["info"]["name"],
        "brief"
    );
    let verbs = of_type(&log, "verb");
    assert!(verbs.iter().any(|verb| verb["name"] == "brief"));
    // The brief asked about a finished conversation: its last message is the
    // question, after the parent's own answer.
    let brief: Vec<Recorded> = mock.requests().into_iter().filter(is_brief).collect();
    let messages = brief[0].messages();
    assert_eq!(messages[messages.len() - 2]["role"], "assistant");
    root.kill().await;
}

#[tokio::test]
async fn only_the_root_writes_the_herdr_status_and_a_long_answer_settles() {
    let long = "x".repeat(100_000);
    let long_for_mock = long.clone();
    let mock = Mock::start_routed(vec![], move |request| {
        let task = task_of(request);
        let done = !tool_results(request).is_empty();
        Some(match (task.as_str(), done) {
            ("ROOT", false) => bash("call_a", &child_cmd("", "CHILD-A work")),
            ("ROOT", true) => say("root done"),
            (a, false) if a.starts_with("CHILD-A") => {
                bash("call_b", &child_cmd("", "CHILD-B leaf"))
            }
            (a, true) if a.starts_with("CHILD-A") => say("A done"),
            (b, _) if b.starts_with("CHILD-B") => say(&long_for_mock),
            _ => return None,
        })
    })
    .await;
    let family = Family::new("");
    // Not `--json`: this is the run that may write OSC status records.
    let out = family
        .sandbox
        .kelir(&mock.base_url(), &["run", "ROOT"], &[("KELIR_HERDR", "1")])
        .await;
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("\u{1b}]7501;"),
        "the root writes its status: {:?}",
        out.stdout
    );
    for (id, task, log) in family.by_task() {
        if task.starts_with("CHILD") {
            for event in &log {
                let text = event.to_string();
                // The record is the OSC sequence `ESC ] 7501 ;`. Matching the bare
                // digits "7501" was a flake: a pid, a byte count or a temp-dir
                // name in a child's log contains them now and then (STEER-9,
                // seen once in five loaded runs).
                assert!(
                    !text.contains("\u{1b}]7501") && !text.contains("\\u001b]7501"),
                    "a child wrote a status record: {id}"
                );
            }
        }
    }
    let (_, root) = family.session_for("ROOT");
    let (b_id, _) = family.session_for("CHILD-B");
    // The grandchild's 100 KB answer reached the root's registry (cut to what a
    // request line carries) and the middle agent's whole tool result.
    let settled = calls_of(&root, "call_settled", "child");
    assert_eq!(settled.len(), 2, "{settled:?}");
    let child_starts = calls_of(&root, "call_started", "child");
    let deep = settled
        .iter()
        .find(|event| {
            child_starts.iter().any(|start| {
                start["id"] == event["id"] && start["info"]["session"] == b_id.as_str()
            })
        })
        .unwrap();
    assert_eq!(deep["outcome"], "done");
    let (_, a) = family.session_for("CHILD-A");
    let call_b = a
        .iter()
        .find(|event| event["type"] == "call_settled" && event["id"] == "call_b")
        .unwrap();
    assert_eq!(call_b["bytes"].as_u64().unwrap() as usize, long.len() + 1);
}
