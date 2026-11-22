//! End-to-end: `kelir ctl <session> status`, token tallies, warnings.
//!
//! The real binary against the scripted mock. `status` is read-only, so it is
//! asked of a live session in whatever state it is in: before the first
//! request, mid-request, in a retry wait, paused.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use std::sync::Arc;

use crate::support::*;
use serde_json::{Value, json};

/// A text turn with a prompt of a chosen size.
fn prompt_turn(text: &str, prompt: u64) -> Vec<Value> {
    vec![
        delta(text),
        finish("stop"),
        json!({"choices": [], "usage": {
            "prompt_tokens": prompt, "completion_tokens": 5
        }}),
    ]
}

async fn status(session: &LiveSession) -> Value {
    let answer = session.ctl(&["status", "--json"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let response = answer.response();
    assert_eq!(response["ok"], true, "{response}");
    response["state"].clone()
}

fn of_type<'a>(log: &'a [Value], event_type: &str) -> Vec<&'a Value> {
    log.iter().filter(|e| e["type"] == event_type).collect()
}

/// Before any answer (the request is in flight) the status is complete with
/// no tokens; after the step the numbers are in, and the human text is the default.
#[tokio::test]
async fn status_before_and_after_a_step() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![Reply::Gated(
        gate.clone(),
        sse(&text_turn("hello", 100)),
    )])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    let before = status(&session).await;
    assert_eq!(before["tokens"]["total"], 0, "{before}");
    assert_eq!(before["requests"], 0);
    assert_eq!(before["context"]["last_prompt_tokens"], Value::Null);
    assert_eq!(before["last_error"], Value::Null);
    assert_eq!(before["retry"], Value::Null);
    assert_eq!(before["model"], "test/model");

    gate.notify_one();
    session.wait_for("turn_ended").await;
    let after = status(&session).await;
    assert_eq!(after["requests"], 1, "{after}");
    assert_eq!(after["tokens"]["prompt"], 200);
    assert_eq!(after["tokens"]["completion"], 8);
    assert_eq!(after["tokens"]["cached"], 100);
    assert_eq!(after["context"]["last_prompt_tokens"], 200);
    assert_eq!(after["context"]["window"], 262144);
    assert_eq!(after["cache"]["last_hit_ratio"], 0.5);
    assert_eq!(after["turn"], 1);
    assert!(after["session_steps"].as_u64().unwrap() >= 1);

    let text = session.ctl(&["status"]).await;
    assert_eq!(text.code, 0, "{}{}", text.stdout, text.stderr);
    assert!(text.stdout.contains("test/model"), "{}", text.stdout);
    assert!(
        !text.stdout.trim_start().starts_with('{'),
        "{}",
        text.stdout
    );
    // `status` is read-only: it leaves nothing in the log.
    let log = session.log();
    assert!(
        !log.iter()
            .any(|e| e.to_string().contains("\"status\"") && e["type"] == "verb"),
        "status was logged"
    );
    let (code, stderr) = session.end().await;
    assert_eq!(code, 0, "{stderr}");
}

/// A retry in progress shows in `status`.
#[tokio::test]
async fn status_shows_a_retry_in_progress() {
    let mock = Mock::start(vec![Reply::HttpWith(
        429,
        vec![("Retry-After".into(), "3600".into())],
        "slow down".into(),
    )])
    .await;
    let mut session = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "retry_max_wait = 7200\n",
    )
    .await;
    session.wait_for("error").await;
    let state = status(&session).await;
    assert_eq!(state["state"], "retrying", "{state}");
    assert_eq!(state["retry"]["status"], 429, "{state}");
    assert!(state["retry"]["remaining_ms"].as_u64().unwrap() > 1000);
    assert_eq!(state["last_error"]["retryable"], true);
    assert_eq!(state["last_error"]["recovered"], false);
    let text = session.ctl(&["status"]).await;
    assert!(text.stdout.contains("429"), "{}", text.stdout);
    session.ctl(&["stop", "enough"]).await;
    session.wait_for("turn_ended").await;
    session.end().await;
}

/// The waiting budget spent: the turn is paused and `status` says why.
#[tokio::test]
async fn status_names_why_a_turn_is_paused() {
    let mock = Mock::start(vec![
        Reply::Http(429, "x".into()),
        Reply::Http(429, "x".into()),
        Reply::Http(429, "x".into()),
    ])
    .await;
    let mut session = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "retry_base_ms = 50\nretry_cap_ms = 100\nretry_max_attempts = 3\nretry_max_wait = 5\n",
    )
    .await;
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "paused");
    let state = status(&session).await;
    assert_eq!(state["state"], "paused", "{state}");
    assert!(state["pause_reason"].is_string(), "{state}");
    assert_eq!(state["retry"], Value::Null);
    session.end().await;
}

/// The prompt warning fires once per crossing: two requests over the threshold
/// make one warning; falling back under re-arms it.
#[tokio::test]
async fn the_prompt_warning_fires_once_per_crossing() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&prompt_turn("a", 500))),
        Reply::Sse(sse(&prompt_turn("b", 600))),
        Reply::Sse(sse(&prompt_turn("c", 50))),
        Reply::Sse(sse(&prompt_turn("d", 700))),
    ])
    .await;
    let mut session = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "warn_prompt_tokens = 400\n",
    )
    .await;
    session.wait_for("turn_ended").await;
    for word in ["two", "three", "four"] {
        session.ctl(&["say", word]).await;
        session.wait_for("turn_ended").await;
    }
    let log = session.log();
    let warnings = of_type(&log, "note");
    assert_eq!(warnings.len(), 2, "{warnings:#?}");
    assert_eq!(warnings[0]["kind"], "prompt_tokens");
    assert!(
        warnings[0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("500"))
    );
    assert!(
        warnings[1]["text"]
            .as_str()
            .is_some_and(|text| text.contains("700"))
    );
    for request in mock.requests() {
        assert!(
            !request.body.to_string().contains("warning"),
            "a warning reached the model"
        );
    }
    session.end().await;
}
