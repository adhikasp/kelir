//! STEER-8 review: adversarial tests added by the reviewer (no product code touched).

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use crate::support::*;
use serde_json::{Value, json};

fn sized_turn(text: &str, prompt: u64) -> Vec<Value> {
    vec![
        delta(text),
        finish("stop"),
        json!({"choices": [], "usage": {
            "prompt_tokens": prompt, "completion_tokens": 5
        }}),
    ]
}

fn warnings(log: &[Value]) -> Vec<&Value> {
    log.iter().filter(|e| e["type"] == "note").collect()
}

async fn status(session: &LiveSession) -> Value {
    let answer = session.ctl(&["status", "--json"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    answer.response()["state"].clone()
}

/// A resumed session whose log already carries the warning does not repeat it
/// while the prompt stays over the line, and the stderr of a plain run is the
/// only other place it speaks.
#[tokio::test]
async fn a_resume_does_not_repeat_a_warning_the_log_already_owes() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&sized_turn("a", 500))),
        Reply::Sse(sse(&sized_turn("b", 600))),
    ])
    .await;
    let sandbox = Sandbox::new("warn_prompt_tokens = 400\n");
    let first = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "go"], &[])
        .await;
    assert_eq!(first.code, 0, "{}", first.stderr);
    let id = sandbox.only_session();
    assert_eq!(warnings(&sandbox.log(&id)).len(), 1);
    let second = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "--resume", id.as_str(), "more"],
            &[],
        )
        .await;
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert_eq!(
        warnings(&sandbox.log(&id)).len(),
        1,
        "a resume repeated the warning"
    );
}

/// A running (not promoted) tool shows in running_calls; a promoted job counts
/// in bg_jobs until it settles or is cancelled.
#[tokio::test]
async fn status_sees_running_calls_and_bg_jobs() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_p",
            "bash",
            &json!({"command": "sleep 60"}),
        ))),
        Reply::Sse(sse(&text_turn("moving on", 0))),
        Reply::Sse(sse(&text_turn("job gone", 0))),
    ])
    .await;
    let mut s = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "promote_after = 3\n",
    )
    .await;
    s.wait_for("call_started").await;
    let st = status(&s).await;
    assert_eq!(st["running_calls"].as_array().unwrap().len(), 1, "{st}");
    assert_eq!(st["bg_jobs"], 0);
    let settled = s.wait_for("call_settled").await;
    assert_eq!(settled["outcome"], "promoted", "{settled}");
    let started = s.wait_for("call_started").await;
    let id = started["id"].as_str().unwrap().to_string();
    let st = status(&s).await;
    assert_eq!(st["bg_jobs"], 1, "{st}");
    let c = s.ctl(&["cancel", &id]).await;
    assert_eq!(c.code, 0, "{}{}", c.stdout, c.stderr);
    s.wait_for("call_settled").await;
    let st = status(&s).await;
    assert_eq!(st["bg_jobs"], 0, "{st}");
    let _ = s.end().await;
}

/// `status` with a wrong token is refused like any verb (no state leak), and
/// the refusal is not logged as a status read.
#[tokio::test]
async fn status_needs_the_token_and_leaks_nothing_without_it() {
    let mock = Mock::start(vec![Reply::Sse(sse(&sized_turn("a", 10)))]).await;
    let mut s = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    s.wait_for("turn_ended").await;
    let tok = s.session_dir.join("token");
    let saved = std::fs::read(&tok).ok();
    assert!(saved.is_some(), "no token file at {tok:?}");
    {
        std::fs::write(&tok, "not-the-token").unwrap();
        let answer = s.ctl(&["status", "--json"]).await;
        assert_ne!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
        assert!(!answer.stdout.contains("tokens"), "{}", answer.stdout);
        std::fs::write(&tok, saved.unwrap()).unwrap();
    }
    let _ = s.end().await;
}
