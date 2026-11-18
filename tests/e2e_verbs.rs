//! M1 end-to-end: verbs over the session socket.
//!
//! The real binary runs as a `--keep` session against the scripted mock, and
//! `kelir ctl` drives it from a second process, exactly as a human or a script
//! would. Assertions are on the event log, on the mock's requests, and on the
//! session process itself.

mod support;

use std::time::{Duration, Instant};

use serde_json::Value;
use support::*;

fn of_type<'a>(log: &'a [Value], event_type: &str) -> Vec<&'a Value> {
    log.iter()
        .filter(|event| event["type"] == event_type)
        .collect()
}

fn seq(event: &Value) -> u64 {
    event["seq"].as_u64().expect("seq")
}

/// `say` without `--now` waits for the next boundary: it arrives as a user
/// message after the tool result, never between the call and its result.
#[tokio::test]
async fn say_mid_turn_is_in_the_next_request() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_1",
            "bash",
            &serde_json::json!({"command": "sleep 1"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("tool_started").await;

    let answer = session.ctl(&["say", "also print the time"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let response = answer.response();
    assert_eq!(response["ok"], true);
    assert_eq!(response["verb"], "say");
    assert_eq!(response["applies"], "queued");

    let settled = session.wait_for("tool_settled").await;
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
    let started = of_type(&log, "tool_started")[0];
    let settled = of_type(&log, "tool_settled")[0];
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
    session.wait_for("tool_started").await;

    let started = Instant::now();
    let answer = session
        .ctl(&["say", "--now", "change of plan: just print hello"])
        .await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let settled = session.wait_for("tool_settled").await;
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
    session.wait_for("tool_started").await;

    let answer = session.ctl(&["stop", "enough for now"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "stopped");
    assert_eq!(mock.request_count(), 1, "a stopped turn asks nothing more");

    let log = session.log();
    let settled = of_type(&log, "tool_settled")[0];
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
                serde_json::json!({"command": "sleep 1; printf 'b\\n'"}),
            ),
        ))),
        Reply::Sse(sse(&text_turn("a was stopped, b finished", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("tool_started").await;
    session.wait_for("tool_started").await;

    let answer = session
        .ctl(&["stop", "--call", "call_a", "wrong call"])
        .await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);

    let first = session.wait_for("tool_settled").await;
    let second = session.wait_for("tool_settled").await;
    assert_eq!(first["call_id"], "call_a");
    assert_eq!(first["outcome"], "stopped");
    assert_eq!(first["result"], "stopped by user: wrong call");
    assert_eq!(second["call_id"], "call_b");
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
    session.wait_for("tool_started").await;

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
            &serde_json::json!({"command": "sleep 1"}),
        ))),
        Reply::Sse(sse(&text_turn("done on the new model", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("tool_started").await;

    let answer = session.ctl(&["model", "other/cheap-model"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    assert_eq!(answer.response()["applies"], "next_step");

    session.wait_for("tool_settled").await;
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

/// A swap can name another configured provider: `provider/model`.
#[tokio::test]
async fn model_swap_can_change_the_provider() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_1",
            "bash",
            &serde_json::json!({"command": "sleep 1"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let extra = "[providers.alt]\nkind = \"custom\"\nbase_url = \"http://example.invalid/v1\"\n";
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], extra).await;
    session.wait_for("tool_started").await;

    let answer = session.ctl(&["model", "alt/some-model"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    session.wait_for("tool_settled").await;
    session.wait_for("turn_ended").await;

    let log = session.log();
    let steps = of_type(&log, "step_started");
    assert_eq!(steps[1]["provider"], "alt");
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
            &serde_json::json!({"command": "sleep 1"}),
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
    session.wait_for("tool_started").await;

    let answer = session.ctl(&["tools", "--disable", "bash"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);

    session.wait_for("tool_settled").await;
    let disabled = session.wait_for("tool_settled").await;
    assert_eq!(disabled["call_id"], "call_2");
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
    for _ in 0..100 {
        if mock.request_count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        mock.request_count(),
        1,
        "the request never reached the mock"
    );

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

/// `end` while idle: the process exits 0 and logs `session_ended`.
#[tokio::test]
async fn end_ends_the_session() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("hi", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;

    let answer = session.ctl(&["end"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let (code, stderr) = session.finish().await;
    assert_eq!(code, 0, "{stderr}");
}

/// `end` in the middle of a turn kills the call, ends the turn as `stopped` and
/// exits.
#[tokio::test]
async fn end_mid_turn_stops_the_call_and_exits() {
    let mock = Mock::start(vec![Reply::Sse(sse(&tool_turn(
        "call_long",
        "bash",
        &serde_json::json!({"command": "sleep 30"}),
    )))])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("tool_started").await;

    let answer = session.ctl(&["end"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let (code, stderr) = session.finish().await;
    assert_eq!(code, 0, "{stderr}");
}

/// A verb for a session that is gone is a clear error, not a hang.
#[tokio::test]
async fn a_verb_to_a_dead_session_errors_cleanly() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("hi", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let id = session.id.clone();
    // The session dies without saying goodbye; the state dir still lists it.
    session.kill().await;

    let answer = ctl_in(&session.state, &session.cwd, &session.config, &[&id, "end"]).await;
    assert_eq!(answer.code, 1, "{}", answer.stdout);
    assert!(answer.stderr.contains("not running"), "{}", answer.stderr);
    // `latest` resolves to the dead one and reports it, instead of hanging.
    let answer = ctl_in(
        &session.state,
        &session.cwd,
        &session.config,
        &["latest", "end"],
    )
    .await;
    assert_eq!(answer.code, 1);
    assert!(answer.stderr.contains("not running"), "{}", answer.stderr);
    // A session that never existed.
    let answer = ctl_in(
        &session.state,
        &session.cwd,
        &session.config,
        &["ses_00000000000000000000000000", "end"],
    )
    .await;
    assert_eq!(answer.code, 1);
    assert!(
        answer.stderr.contains("no session matches"),
        "{}",
        answer.stderr
    );
    // An empty state dir.
    let empty = tempfile::tempdir().unwrap();
    let answer = ctl_in(
        empty.path(),
        &session.cwd,
        &session.config,
        &["latest", "end"],
    )
    .await;
    assert_eq!(answer.code, 1);
    assert!(
        answer.stderr.contains("no sessions yet"),
        "{}",
        answer.stderr
    );
}

/// Two clients at once both get their answers.
#[tokio::test]
async fn two_clients_can_send_at_once() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("first", 0))),
        Reply::Sse(sse(&text_turn("second", 0))),
        Reply::Sse(sse(&text_turn("third", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;

    let first = session.ctl(&["say", "one"]);
    let second = session.ctl(&["say", "two"]);
    let (first, second) = tokio::join!(first, second);
    assert_eq!(first.code, 0, "{}{}", first.stdout, first.stderr);
    assert_eq!(second.code, 0, "{}{}", second.stdout, second.stderr);
    assert_eq!(first.response()["ok"], true);
    assert_eq!(second.response()["ok"], true);
    assert_ne!(first.response()["seq"], second.response()["seq"]);

    // Both were written to the log, and the log's seq stayed monotonic even
    // though two clients raced.
    let log = session.log();
    let verbs = of_type(&log, "verb");
    assert_eq!(verbs.len(), 2);
    let seqs: Vec<u64> = log.iter().map(seq).collect();
    assert!(
        seqs.windows(2).all(|pair| pair[0] < pair[1]),
        "the log's seq must stay monotonic: {seqs:?}"
    );

    // Both messages reach a request, in the end.
    session.wait_for("step_started").await;
    let _ = session.ctl(&["end"]).await;
    let _ = session.finish().await;
    let seen = mock.requests().iter().any(|request| {
        let texts: Vec<&str> = request
            .messages()
            .iter()
            .filter(|message| message["role"] == "user")
            .filter_map(|message| message["content"].as_str())
            .collect();
        texts.contains(&"one") && texts.contains(&"two")
    });
    assert!(seen, "both messages must reach the model");
}

/// A bad verb is refused by `ctl` itself, before it ever reaches the session.
#[tokio::test]
async fn a_bad_verb_never_reaches_the_session() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("hi", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;

    let answer = session.ctl(&["frobnicate"]).await;
    assert_eq!(answer.code, 2, "{}", answer.stdout);
    assert!(answer.stderr.contains("unknown verb"), "{}", answer.stderr);
    assert_eq!(answer.stdout.trim(), "", "nothing is printed for a refusal");
    // `say` with no text, and a tool that does not exist.
    assert_eq!(session.ctl(&["say"]).await.code, 2);
    assert_eq!(session.ctl(&["tools", "--disable", "spawn"]).await.code, 2);
    assert_eq!(
        of_type(&session.log(), "verb").len(),
        0,
        "a request that never left ctl is not a session verb"
    );

    session.end().await;
}

/// A non-`--keep` run still honours a queued `later`: it runs the second turn
/// and only then exits.
#[tokio::test]
async fn a_headless_run_drains_queued_later_messages_before_exiting() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_1",
            "bash",
            &serde_json::json!({"command": "sleep 1"}),
        ))),
        Reply::Sse(sse(&text_turn("first done", 0))),
        Reply::Sse(sse(&text_turn("second done", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--json", "go"], "").await;
    session.wait_for("tool_started").await;
    let answer = session.ctl(&["later", "one more turn"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);

    let first = session.wait_for("turn_ended").await;
    assert_eq!(first["outcome"], "done");
    let user = session.wait_for("user").await;
    assert_eq!(user["delivery"], "later");
    let second = session.wait_for("turn_ended").await;
    assert_eq!(second["outcome"], "done");

    let (code, stderr) = session.finish().await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(mock.request_count(), 3);
}

/// Every bash child sees the session socket, so `kelir mcp`/`kelir bg` can
/// reach the session process.
#[tokio::test]
async fn every_bash_child_gets_kelir_sock() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_1",
            "bash",
            &serde_json::json!({"command": "printf '%s' \"$KELIR_SOCK\""}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "go"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let id = run.one_event("session_started")["id"].as_str().unwrap();
    let result = run.one_event("tool_settled")["result"]
        .as_str()
        .unwrap()
        .trim()
        .to_string();
    if cfg!(windows) {
        assert_eq!(result, format!(r"\\.\pipe\kelir-{id}"));
    } else {
        assert!(result.ends_with("sock"), "{result}");
        assert!(result.contains(id), "{result}");
    }
}
