//! verbs session tests: later_starts_a_second_turn, model_swap_applies_at_the_next, model_swap_can_change_the_prov, say_during_a_stream_gets_anoth, say_mid_turn_is_in_the_next_re, say_now_stops_the_running_call, say_while_idle_starts_a_turn, stop_call_stops_one_of_two_par, stop_call_with_an_unknown_id_i, stop_ends_the_turn, tools_disable_returns_disabled.
#![allow(clippy::pedantic, clippy::nursery)]

use std::time::{Duration, Instant};

use crate::support::*;
use serde_json::Value;

use super::verbs_core::*;

/// `say` without `--now` waits for the next boundary: it arrives as a user
/// message after the tool result, never between the call and its result.
#[tokio::test]
async fn say_mid_turn_is_in_the_next_request() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_1",
            "bash",
            &serde_json::json!({"command": HOLD}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("call_started").await;

    let answer = session.ctl(&["say", "also print the time"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let response = answer.response();
    assert_eq!(response["ok"], true);
    assert_eq!(response["verb"], "say");
    assert_eq!(response["applies"], "queued");
    session.release();

    let settled = session.wait_for("call_settled").await;
    assert_eq!(settled["outcome"], "ok");
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "done");

    let requests = mock.requests();
    assert_eq!(requests.len(), 2, "one tool step, one answering step");
    assert_eq!(
        requests[1].roles(),
        vec!["system", "user", "assistant", "tool", "user"],
        "the message must come after the tool result"
    );
    assert_eq!(requests[1].messages()[4]["content"], "also print the time");

    let log = session.log();
    let started = of_type(&log, "call_started")[0];
    let settled = of_type(&log, "call_settled")[0];
    let user = of_type(&log, "user")
        .into_iter()
        .find(|event| event["text"] == "also print the time")
        .expect("the injected user event");
    assert!(
        seq(started) < seq(settled) && seq(settled) < seq(user),
        "the message landed between the call and its result"
    );
    assert_eq!(user["delivery"], "next");
    // The verb itself is logged too, before the message it produced.
    let verb = of_type(&log, "verb")[0];
    assert_eq!(verb["name"], "say");
    assert_eq!(verb["from"], "ctl");
    assert!(seq(verb) < seq(user));

    session.end().await;
}

/// `say --now` stops a running `sleep` within the 2 s the task asks for, and
/// the next request carries the `stopped` result and then the message.
#[tokio::test]
async fn say_now_stops_the_running_call_within_two_seconds() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_long",
            "bash",
            &serde_json::json!({"command": "sleep 30"}),
        ))),
        Reply::Sse(sse(&text_turn("just hello", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("call_started").await;

    let answer = session
        .ctl(&["say", "--now", "change of plan: just print hello"])
        .await;
    // The clock starts at the acknowledgement: how long the `ctl` process takes
    // to start on a loaded box is not the session's latency (STEER-9).
    let started = Instant::now();
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let settled = session.wait_for("call_settled").await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "say --now took {elapsed:?} to settle the sleeping call"
    );
    assert_eq!(settled["outcome"], "stopped");
    let result = settled["result"].as_str().unwrap();
    assert!(result.starts_with("stopped by user"), "{result}");

    let ended = session.wait_for("turn_ended").await;
    assert_eq!(
        ended["outcome"], "done",
        "the turn continues after say --now"
    );

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let messages = requests[1].messages();
    assert_eq!(
        requests[1].roles(),
        vec!["system", "user", "assistant", "tool", "user"]
    );
    assert!(
        messages[3]["content"]
            .as_str()
            .unwrap()
            .starts_with("stopped by user"),
        "{}",
        messages[3]["content"]
    );
    assert_eq!(messages[4]["content"], "change of plan: just print hello");
    assert!(messages[3]["tool_call_id"].as_str().is_some());

    session.end().await;
}

/// `stop` ends the turn, keeps the partial work in the log, and asks the model
/// nothing more.
#[tokio::test]
async fn stop_ends_the_turn() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_long",
            "bash",
            &serde_json::json!({"command": "sleep 30"}),
        ))),
        Reply::Sse(sse(&text_turn("never asked", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("call_started").await;

    let answer = session.ctl(&["stop", "enough for now"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "stopped");
    assert_eq!(mock.request_count(), 1, "a stopped turn asks nothing more");

    let log = session.log();
    let settled = of_type(&log, "call_settled")[0];
    assert_eq!(settled["outcome"], "stopped");
    assert!(
        settled["result"]
            .as_str()
            .unwrap()
            .contains("stopped by user"),
        "{}",
        settled["result"]
    );
    let verb = of_type(&log, "verb")[0];
    assert_eq!(verb["name"], "stop");
    assert_eq!(verb["args"]["reason"], "enough for now");
    assert_eq!(
        of_type(&log, "session_ended").len(),
        0,
        "the session lives on"
    );

    session.end().await;
}

/// `stop --call` on one of two parallel calls: that one is `stopped`, the other
/// finishes normally, and the turn continues.
#[tokio::test]
async fn stop_call_stops_one_of_two_parallel_calls() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&two_tool_turn(
            ("call_a", "bash", serde_json::json!({"command": "sleep 30"})),
            (
                "call_b",
                "bash",
                serde_json::json!({"command": hold_then("printf 'b\\n'")}),
            ),
        ))),
        Reply::Sse(sse(&text_turn("a was stopped, b finished", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("call_started").await;
    session.wait_for("call_started").await;

    let answer = session
        .ctl(&["stop", "--call", "call_a", "wrong call"])
        .await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    // `call_b` is held until the stop has been answered, so `call_a` settles
    // first by construction, not by the race between a stopwatch and a ctl.
    session.release();

    let first = session.wait_for("call_settled").await;
    let second = session.wait_for("call_settled").await;
    assert_eq!(first["id"], "call_a");
    assert_eq!(first["outcome"], "stopped");
    assert_eq!(first["result"], "stopped by user: wrong call");
    assert_eq!(second["id"], "call_b");
    assert_eq!(second["outcome"], "ok");
    assert_eq!(second["result"], "b\n");

    let ended = session.wait_for("turn_ended").await;
    assert_eq!(
        ended["outcome"], "done",
        "the turn continues after stop --call"
    );

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let tool_messages: Vec<&Value> = requests[1]
        .messages()
        .iter()
        .filter(|message| message["role"] == "tool")
        .collect();
    assert_eq!(tool_messages.len(), 2);
    assert_eq!(tool_messages[0]["content"], "stopped by user: wrong call");
    assert_eq!(tool_messages[1]["content"], "b\n");

    session.end().await;
}

/// A call id that is not running is refused, not silently ignored.
#[tokio::test]
async fn stop_call_with_an_unknown_id_is_refused() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_1",
            "bash",
            &serde_json::json!({"command": "sleep 30"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("call_started").await;

    let answer = session.ctl(&["stop", "--call", "call_nope"]).await;
    assert_eq!(answer.code, 1, "{}", answer.stdout);
    let response = answer.response();
    assert_eq!(response["ok"], false);
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("no running call"),
        "{response}"
    );
    // The rejected verb is still in the log, marked as such.
    let log = session.log();
    let verb = of_type(&log, "verb")[0];
    assert_eq!(verb["boundary"], "rejected");
    assert!(verb["error"].as_str().unwrap().contains("no running call"));

    let _ = session.ctl(&["stop", "end it"]).await;
    session.wait_for("turn_ended").await;
    session.end().await;
}

/// `model` swaps the provider/model for the rest of the session, applied at the
/// next step and recorded in `step_started`.
#[tokio::test]
async fn model_swap_applies_at_the_next_step() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_1",
            "bash",
            &serde_json::json!({"command": HOLD}),
        ))),
        Reply::Sse(sse(&text_turn("done on the new model", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("call_started").await;

    let answer = session.ctl(&["model", "other/cheap-model"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    assert_eq!(answer.response()["applies"], "next_step");
    session.release();

    session.wait_for("call_settled").await;
    session.wait_for("turn_ended").await;

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body["model"], "test/model");
    assert_eq!(
        requests[1].body["model"], "other/cheap-model",
        "the swap must apply at the next step"
    );

    let log = session.log();
    let steps = of_type(&log, "step_started");
    assert_eq!(steps[0]["model"], "test/model");
    assert_eq!(steps[1]["model"], "other/cheap-model");
    // The prefix is untouched: only the model in the body changed.
    let first = requests[0].body["messages"].as_array().unwrap();
    let second = requests[1].body["messages"].as_array().unwrap();
    assert_eq!(&second[..first.len()], first.as_slice());

    session.end().await;
}

/// A swap can name the other provider: `provider/model` (STEER-2: exactly two).
#[tokio::test]
async fn model_swap_can_change_the_provider() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_1",
            "bash",
            &serde_json::json!({"command": HOLD}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    // The swap takes `opencode-go`'s own key source: `KELIR_GO_API_KEY`, which
    // the test harness sets, so no `opencode` process is started here.
    let extra = "";
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], extra).await;
    session.wait_for("call_started").await;

    let answer = session.ctl(&["model", "opencode-go/some-model"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    session.release();
    session.wait_for("call_settled").await;
    session.wait_for("turn_ended").await;

    let log = session.log();
    let steps = of_type(&log, "step_started");
    assert_eq!(steps[1]["provider"], "opencode-go");
    assert_eq!(steps[1]["model"], "some-model");

    session.end().await;
}

/// `tools --disable bash`: the next call comes back `disabled by user`, while
/// the `tools` array in the request is byte-identical.
#[tokio::test]
async fn tools_disable_returns_disabled_by_user_and_keeps_the_tool_list() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_1",
            "bash",
            &serde_json::json!({"command": HOLD}),
        ))),
        Reply::Sse(sse(&tool_turn(
            "call_2",
            "bash",
            &serde_json::json!({"command": "printf 'should not run\\n'"}),
        ))),
        Reply::Sse(sse(&text_turn("bash is off", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("call_started").await;

    let answer = session.ctl(&["tools", "--disable", "bash"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    session.release();

    session.wait_for("call_settled").await;
    let disabled = session.wait_for("call_settled").await;
    assert_eq!(disabled["id"], "call_2");
    assert_eq!(disabled["outcome"], "disabled");
    assert!(
        disabled["result"]
            .as_str()
            .unwrap()
            .contains("disabled by user"),
        "{}",
        disabled["result"]
    );
    assert_eq!(disabled["out_path"], "", "a disabled call runs nothing");
    session.wait_for("turn_ended").await;

    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert_eq!(
            request.body["tools"], requests[0].body["tools"],
            "the tool list sent to the provider must never change"
        );
    }

    // `--enable` brings it back.
    let answer = session.ctl(&["tools", "--enable", "bash"]).await;
    assert_eq!(answer.code, 0, "{}", answer.stdout);

    session.end().await;
}

/// `later` is queued for the end of the turn and starts a second one.
#[tokio::test]
async fn later_starts_a_second_turn() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("first answer", 0))),
        Reply::Sse(sse(&text_turn("second answer", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "done");

    let answer = session.ctl(&["later", "and now do this"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    assert_eq!(answer.response()["applies"], "turn_end");

    let user = session.wait_for("user").await;
    assert_eq!(user["text"], "and now do this");
    assert_eq!(user["delivery"], "later");
    let step = session.wait_for("step_started").await;
    assert_eq!(step["step"], 2, "a new turn keeps counting steps");
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "done");

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].messages().last().unwrap()["content"],
        "and now do this"
    );

    session.end().await;
}

/// A `say` while the session is idle starts a new turn in the same process.
#[tokio::test]
async fn say_while_idle_starts_a_turn() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("first", 0))),
        Reply::Sse(sse(&text_turn("second", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;

    let answer = session.ctl_latest(&["say", "keep going"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);

    let user = session.wait_for("user").await;
    assert_eq!(user["text"], "keep going");
    session.wait_for("step_started").await;
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "done");
    assert_eq!(mock.request_count(), 2);

    session.end().await;
}

/// A `say` that arrives while a response is streaming waits for the boundary
/// and gets another step.
#[tokio::test]
async fn say_during_a_stream_gets_another_step() {
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        Reply::Gated(gate.clone(), sse(&text_turn("first answer", 0))),
        Reply::Sse(sse(&text_turn("second answer", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("step_started").await;
    eventually(
        "the first request to reach the mock",
        || mock.request_count() >= 1,
        || session.dump(),
    )
    .await;
    assert_eq!(mock.request_count(), 1, "one request, held by the gate");

    let answer = session.ctl(&["say", "one more thing"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    gate.notify_one();

    let assistant = session.wait_for("assistant").await;
    assert_eq!(assistant["text"], "first answer");
    let user = session.wait_for("user").await;
    assert_eq!(user["text"], "one more thing");
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "done");

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].messages().last().unwrap()["content"],
        "one more thing"
    );

    session.end().await;
}
