//! Regression: the built-in hint's scorer call must carry the same provider session header as the
//! session client. Without it `opencode-go` answers 400 MissingSessionID and the gate fails open.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use crate::support::*;
use serde_json::Value;

const GO_HINT: &str = "provider = 'opencode-go'\nmodel = 'go/model'\n\
    [gates.hint]\nbuiltin = 'hint'\nmodel = 'go/model'\nprovider = 'opencode-go'\n";

/// The mock rejects the scorer's POST exactly as `opencode-go` rejects one without a session id:
/// only the streaming step is answered normally. The first request is the scorer's (a gate runs
/// before the turn's request), so the session header is what decides whether a hint appears.
#[tokio::test]
async fn the_hint_scorer_sends_the_go_session_header() {
    let project = super::hint_core::Project::new();
    let mock = Mock::start_routed(
        vec![
            Reply::Sse(sse(&text_turn("ok", 0))),
            Reply::Sse(sse(&text_turn("ok", 0))),
        ],
        |request| {
            if request.body["stream"] == false {
                if request.header("x-opencode-session").is_none() {
                    return Some(Reply::Http(400, "MissingSessionID".into()));
                }
                let content = serde_json::to_string(&["sift"]).unwrap();
                return Some(Reply::Sse(
                    serde_json::json!({"choices": [{"message": {"role": "assistant", "content": content}}]}).to_string(),
                ));
            }
            None
        },
    )
    .await;
    let run = run_task(
        &mock,
        &[
            "run",
            "--json",
            "--cwd",
            &project.path(),
            "this log is too long",
        ],
        &[],
        None,
        GO_HINT,
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let started = run.one_event("session_started");
    let session_id = started["id"]
        .as_str()
        .expect("a session id on session_started");
    let requests = mock.requests();
    assert_eq!(
        requests.len(),
        2,
        "one scoring call, one step: {:#?}",
        requests
    );
    let scorer = &requests[0];
    assert_eq!(
        scorer.body["stream"], false,
        "the first request is the scorer's"
    );
    assert_eq!(
        scorer.header("x-opencode-session"),
        Some(session_id),
        "the scorer must send the provider's session header"
    );
    assert_eq!(requests[1].header("x-opencode-session"), Some(session_id));

    let hints: Vec<&Value> = run
        .events_of("source")
        .into_iter()
        .filter(|event| event["kind"] == "hint")
        .collect();
    assert_eq!(
        hints.len(),
        1,
        "the gate must not fail open: {:#?}",
        run.events
    );
    assert!(
        hints[0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("might be relevant: sift"),
        "{:?}",
        hints[0]
    );
}
