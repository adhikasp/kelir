//! `observe` output versus the body the model received.
#![allow(clippy::pedantic, clippy::nursery)]

use std::time::Duration;

use crate::support::*;
use serde_json::Value;

use super::hint_core::*;

#[tokio::test]
async fn observe_raw_is_the_body_the_mock_received_byte_for_byte() {
    let project = Project::new();
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        Reply::Gated(gate.clone(), sse(&text_turn("first", 0))),
        Reply::Sse(sse(&text_turn("second", 0))),
    ])
    .await;
    let mut live = LiveSession::start(
        &mock,
        &[
            "run",
            "--keep",
            "--json",
            "--cwd",
            &project.path(),
            "look at this",
        ],
        "",
    )
    .await;
    // The first request is in flight (held): nothing has changed since it was
    // built, so what observe says would be sent next is exactly that request.
    live.wait_for("step_started").await;
    for _ in 0..100 {
        if mock.request_count() >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let sandbox = Sandbox::from_live(&live);
    let raw = sandbox
        .kelir(&mock.base_url(), &["observe", &live.id, "--raw"], &[])
        .await;
    assert_eq!(raw.code, 0, "{}", raw.stderr);
    let sent = mock.requests()[0].raw_body.clone();
    assert_eq!(raw.stdout, sent, "observe --raw differs from the wire body");
    gate.notify_one();
    live.wait_for("turn_ended").await;

    // At rest the answer is the next request minus the message that has not
    // been typed yet: a strict prefix of what the mock gets after a `say`.
    let at_rest = sandbox
        .kelir(&mock.base_url(), &["observe", &live.id, "--raw"], &[])
        .await;
    assert_eq!(at_rest.code, 0, "{}", at_rest.stderr);
    let observed: Value = serde_json::from_str(&at_rest.stdout).unwrap();
    let answer = live.ctl(&["say", "and now?"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    live.wait_for("turn_ended").await;
    let next = mock.requests()[1].body.clone();
    let seen = observed["messages"].as_array().unwrap();
    assert_eq!(
        &next["messages"].as_array().unwrap()[..seen.len()],
        &seen[..]
    );
    assert_eq!(observed["tools"], next["tools"]);
    assert_eq!(observed["model"], next["model"]);

    // The human view carries every part, and `ctl observe` is the same read.
    let human = sandbox
        .kelir(&mock.base_url(), &["observe", &live.id], &[])
        .await;
    assert_eq!(human.code, 0, "{}", human.stderr);
    for part in [
        "== sources",
        "== hint ==",
        "== tools",
        "== system",
        "== messages",
        "cache:",
        "estimate:",
    ] {
        assert!(
            human.stdout.contains(part),
            "{part} missing: {}",
            human.stdout
        );
    }
    assert!(human.stdout.contains("look at this"));
    let via_ctl = sandbox
        .kelir(&mock.base_url(), &["ctl", &live.id, "observe", "raw"], &[])
        .await;
    assert_eq!(via_ctl.code, 0, "{}", via_ctl.stderr);
    let after = sandbox
        .kelir(&mock.base_url(), &["observe", &live.id, "--raw"], &[])
        .await;
    assert_eq!(via_ctl.stdout, after.stdout);
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

#[tokio::test]
async fn observe_names_an_unknown_session() {
    let sandbox = Sandbox::new("");
    let out = sandbox
        .kelir(
            "http://127.0.0.1:1/v1",
            &["observe", "ses_nope", "--raw"],
            &[],
        )
        .await;
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stdout.is_empty());
}
