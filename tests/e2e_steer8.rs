//! STEER-8 end-to-end: `kelir ctl <session> status`, cost sources, warnings.
//!
//! The real binary against the scripted mock. `status` is read-only, so it is
//! asked of a live session in whatever state it is in: before the first
//! request, mid-request, in a retry wait, paused.

mod support;

use std::sync::Arc;

use serde_json::{Value, json};
use support::*;

/// A text turn whose usage carries no `cost` (the Go provider's habit).
fn costless_turn(text: &str, prompt: u64, cached: u64) -> Vec<Value> {
    vec![
        delta(text),
        finish("stop"),
        json!({"choices": [], "usage": {
            "prompt_tokens": prompt,
            "completion_tokens": 10,
            "prompt_tokens_details": {"cached_tokens": cached}
        }}),
    ]
}

/// A text turn with a prompt of a chosen size and the provider's own cost.
fn sized_turn(text: &str, prompt: u64) -> Vec<Value> {
    vec![
        delta(text),
        finish("stop"),
        json!({"choices": [], "usage": {
            "prompt_tokens": prompt, "completion_tokens": 5, "cost": 0.001
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

fn first_usage(session: &LiveSession) -> Value {
    session
        .log()
        .into_iter()
        .find(|e| e["type"] == "usage")
        .expect("a usage event")
}

/// Before any answer (the request is in flight) the status is complete with
/// nothing spent and nothing known; after the step the numbers are in, and the
/// human text is the default.
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
    assert_eq!(before["cost"]["usd"], Value::Null, "unknown is not zero");
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
    assert_eq!(after["cost"]["source"], "provider");
    assert_eq!(after["cost"]["usd"], 0.00042);
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

/// No provider cost and a configured price: the estimate, labelled `table`,
/// with cached tokens at the cached rate.
#[tokio::test]
async fn a_priced_model_without_a_reported_cost_is_estimated() {
    let mock = Mock::start(vec![Reply::Sse(sse(&costless_turn("ok", 1000, 500)))]).await;
    let mut session = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "[prices.\"test/model\"]\ninput = 1.0\ncached_input = 0.1\noutput = 2.0\n",
    )
    .await;
    session.wait_for("turn_ended").await;
    let usage = first_usage(&session);
    assert_eq!(usage["cost_source"], "table", "{usage}");
    let expected = (500.0 * 1.0 + 500.0 * 0.1 + 10.0 * 2.0) / 1e6;
    assert!(
        (usage["cost"].as_f64().unwrap() - expected).abs() < 1e-12,
        "{usage}"
    );
    let state = status(&session).await;
    assert_eq!(state["cost"]["source"], "table", "{state}");
    session.end().await;
}

/// No cost reported and no price known: `unknown`, no estimate, `usd` null.
#[tokio::test]
async fn an_unpriced_model_stays_unknown() {
    let mock = Mock::start(vec![Reply::Sse(sse(&costless_turn("ok", 1000, 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let usage = first_usage(&session);
    assert_eq!(usage["cost_source"], "unknown", "{usage}");
    assert_eq!(usage["cost"], 0.0);
    let state = status(&session).await;
    assert_eq!(state["cost"]["usd"], Value::Null, "{state}");
    assert_eq!(state["cost"]["source"], "unknown", "{state}");
    session.end().await;
}

/// A provider that reports a cost beats a configured price.
#[tokio::test]
async fn the_provider_cost_wins_over_a_configured_price() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "[prices.\"test/model\"]\ninput = 99.0\noutput = 99.0\n",
    )
    .await;
    session.wait_for("turn_ended").await;
    let usage = first_usage(&session);
    assert_eq!(usage["cost_source"], "provider", "{usage}");
    assert_eq!(usage["cost"], 0.00042);
    session.end().await;
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
        Reply::Sse(sse(&sized_turn("a", 500))),
        Reply::Sse(sse(&sized_turn("b", 600))),
        Reply::Sse(sse(&sized_turn("c", 50))),
        Reply::Sse(sse(&sized_turn("d", 700))),
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
    let warnings = of_type(&log, "warning");
    assert_eq!(warnings.len(), 2, "{warnings:#?}");
    assert_eq!(warnings[0]["kind"], "prompt_tokens");
    assert_eq!(warnings[0]["value"], 500.0);
    assert_eq!(warnings[1]["value"], 700.0);
    for request in mock.requests() {
        assert!(
            !request.body.to_string().contains("warning"),
            "a warning reached the model"
        );
    }
    session.end().await;
}

/// The cost warning fires once, when the session's known cost passes the line.
#[tokio::test]
async fn the_cost_warning_fires_once() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&sized_turn("a", 10))),
        Reply::Sse(sse(&sized_turn("b", 10))),
        Reply::Sse(sse(&sized_turn("c", 10))),
    ])
    .await;
    let mut session = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "warn_cost = 0.0015\n",
    )
    .await;
    session.wait_for("turn_ended").await;
    for word in ["two", "three"] {
        session.ctl(&["say", word]).await;
        session.wait_for("turn_ended").await;
    }
    let log = session.log();
    let warnings = of_type(&log, "warning");
    assert_eq!(warnings.len(), 1, "{warnings:#?}");
    assert_eq!(warnings[0]["kind"], "cost");
    session.end().await;
}
