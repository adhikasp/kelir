//! Reviewer-added adversarial tests for M1 (see docs/reviews/M1-review.md).
//!
//! These try to break the verbs rather than mirror the implementation. Each test
//! names the finding it proves; a failing test here is a real bug.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]
mod support;

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn settled<'a>(log: &'a [Value], call_id: &str) -> &'a Value {
    log.iter()
        .find(|event| event["type"] == "tool_settled" && event["call_id"] == call_id)
        .unwrap_or_else(|| panic!("no tool_settled for {call_id}"))
}

/// R1: `stop --call A` must leave a still-running sibling B alone. The batch
/// arms a 1.5 s deadline on the first `stop --call` and then aborts every
/// unsettled task when it expires, so a sibling that outlives the grace period
/// is cut off and reported as `error: did not settle`.
#[tokio::test]
async fn stop_call_leaves_a_long_running_sibling_alone() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&two_tool_turn(
            ("call_a", "bash", json!({"command": "sleep 30"})),
            (
                "call_b",
                "bash",
                json!({"command": "sleep 5; printf 'b done\\n'"}),
            ),
        ))),
        Reply::Sse(sse(&text_turn("ok", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("tool_started").await;
    session.wait_for("tool_started").await;
    let started = Instant::now();
    let answer = session.ctl(&["stop", "--call", "call_a", "nope"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    session.wait_for("turn_ended").await;
    let waited = started.elapsed();
    let log = session.log();
    let b = settled(&log, "call_b");
    assert_eq!(
        b["outcome"], "ok",
        "sibling call_b was cut off after {waited:?}: {b}"
    );
    assert_eq!(b["result"], "b done\n");
    assert!(
        waited >= Duration::from_secs(4),
        "the turn ended in {waited:?}, before call_b could have finished"
    );
    session.end().await;
}

/// R2: `stop --call` for a call of the batch that already settled is accepted
/// (it is in `live_calls` until the whole batch ends) and arms the same
/// deadline, so it aborts a different, still running call.
#[tokio::test]
async fn stop_call_on_an_already_settled_call_does_not_harm_the_others() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&two_tool_turn(
            ("call_fast", "bash", json!({"command": "printf 'fast\\n'"})),
            (
                "call_slow",
                "bash",
                json!({"command": "sleep 5; printf 'slow done\\n'"}),
            ),
        ))),
        Reply::Sse(sse(&text_turn("ok", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("tool_started").await;
    session.wait_for("tool_started").await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let answer = session.ctl(&["stop", "--call", "call_fast"]).await;
    // Either a clean refusal (the call is done) or a no-op is acceptable.
    let _ = answer;
    session.wait_for("turn_ended").await;
    let log = session.log();
    let slow = settled(&log, "call_slow");
    assert_eq!(slow["outcome"], "ok", "call_slow was harmed: {slow}");
    assert_eq!(slow["result"], "slow done\n");
    assert_eq!(settled(&log, "call_fast")["result"], "fast\n");
    session.end().await;
}

/// R3: `stop` while the model is still streaming ends the turn cleanly, and the
/// next request (after a later `say`) is well formed: no dangling assistant
/// message, the task and the new message in order.
#[tokio::test]
async fn stop_during_the_stream_leaves_a_clean_log_and_next_request() {
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        Reply::Gated(gate.clone(), sse(&text_turn("never seen", 0))),
        Reply::Sse(sse(&text_turn("second turn", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("step_started").await;
    // `step_started` is logged before the request goes out, so wait until the
    // mock really has it: a `stop` that wins the race leaves the first request
    // unsent, and the assertion on two requests would be about the race.
    let deadline = Instant::now() + CEILING;
    while mock.request_count() == 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(mock.request_count(), 1, "the first request never arrived");
    let answer = session.ctl(&["stop", "enough"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "stopped");
    // The held request is still open on the mock side; let it go. `notify_one`
    // stores a permit when the mock has not reached its `notified()` poll yet,
    // where `notify_waiters` would be lost.
    gate.notify_one();
    let answer = session.ctl(&["say", "carry on"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    session.wait_for("turn_ended").await;
    let requests = mock.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(requests[1].roles(), vec!["system", "user", "user"]);
    assert_eq!(requests[1].messages()[2]["content"], "carry on");
    session.end().await;
}

/// R4: garbage on the socket gets an error line and the connection (and the
/// session) stay usable; a half-written request followed by a hang-up does not
/// disturb the session.
#[tokio::test]
async fn a_malformed_socket_line_is_answered_and_the_session_survives() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("hi", 0))),
        Reply::Sse(sse(&text_turn("hi again", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;

    // A raw socket client, as a script would be: it reads the session's token
    // file (STEER-7) once, and every request it sends carries it.
    let token = kelir::trust::read_token(&session.session_dir).expect("a token file");
    let stream = kelir::ipc::connect(&session.session_dir, &session.id)
        .await
        .expect("connect");
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read);
    write.write_all(b"this is not json\n").await.unwrap();
    let mut answer = String::new();
    lines.read_line(&mut answer).await.unwrap();
    let value: Value = serde_json::from_str(answer.trim()).expect("one JSON error line");
    assert_eq!(value["ok"], false, "{answer}");

    // The same connection still works.
    write
        .write_all(
            format!(
                "{{\"verb\":\"say\",\"args\":{{\"text\":\"still here\"}},\"token\":\"{token}\"}}\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    answer.clear();
    lines.read_line(&mut answer).await.unwrap();
    let value: Value = serde_json::from_str(answer.trim()).unwrap();
    assert_eq!(value["ok"], true, "{answer}");
    session.wait_for("turn_ended").await;
    drop(write);

    // A half line and a hang-up.
    let stream = kelir::ipc::connect(&session.session_dir, &session.id)
        .await
        .expect("connect");
    let (_read, mut write) = tokio::io::split(stream);
    write.write_all(b"{\"verb\":\"sa").await.unwrap();
    drop(write);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let answer = session.ctl(&["say", "after the hang-up"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    session.end().await;
}

/// R5: a `say --now` sent while the model is *streaming* (no call running) must
/// not be lost or reordered: it is delivered before the next request.
#[tokio::test]
async fn say_now_during_the_stream_is_delivered() {
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        Reply::Gated(gate.clone(), sse(&text_turn("first answer", 0))),
        Reply::Sse(sse(&text_turn("second answer", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("step_started").await;
    let answer = session.ctl(&["say", "--now", "urgent"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    tokio::time::sleep(Duration::from_millis(200)).await;
    gate.notify_one();
    session.wait_for("turn_ended").await;
    let requests = mock.requests();
    assert_eq!(requests.len(), 2, "the urgent message got no step");
    assert_eq!(
        requests[1].roles(),
        vec!["system", "user", "assistant", "user"]
    );
    session.end().await;
}
