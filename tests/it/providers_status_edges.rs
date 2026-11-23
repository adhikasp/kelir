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
