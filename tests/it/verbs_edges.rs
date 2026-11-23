//! Verb rejection and edge-case tests.
#![allow(clippy::pedantic, clippy::nursery)]

use std::time::{Duration, Instant};

use crate::support::*;

use super::verbs_core::*;

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
    session.wait_for("call_started").await;

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

    let answer = ctl_in(
        &session.guard,
        &session.state,
        &session.cwd,
        &session.config,
        &[&id, "end"],
    )
    .await;
    assert_eq!(answer.code, 1, "{}", answer.stdout);
    assert!(answer.stderr.contains("not running"), "{}", answer.stderr);
    // `latest` resolves to the dead one and reports it, instead of hanging.
    let answer = ctl_in(
        &session.guard,
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
        &session.guard,
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
        &session.guard,
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

    // Both messages reach a request, in the end. The session starts a turn on
    // the first message it sees, so the second `say` may arrive too late for
    // that step and get its own turn: wait on the events that say both were
    // folded in, then on the turn that carries the last one.
    let mut delivered: Vec<String> = Vec::new();
    while delivered.len() < 2 {
        let event = session.next_event().await;
        if event["type"] == "user" {
            let text = event["text"].as_str().unwrap_or_default();
            if text == "one" || text == "two" {
                delivered.push(text.to_string());
            }
        }
    }
    session.wait_for("turn_ended").await;
    let seen: Vec<String> = mock
        .requests()
        .iter()
        .flat_map(|request| {
            request
                .messages()
                .iter()
                .filter(|message| message["role"] == "user")
                .filter_map(|message| message["content"].as_str().map(str::to_string))
                .collect::<Vec<String>>()
        })
        .collect();
    for text in ["one", "two"] {
        assert!(
            seen.iter().any(|seen| seen == text),
            "the `{text}` message must reach the model: {seen:?}"
        );
    }
    let _ = session.ctl(&["end"]).await;
    let _ = session.finish().await;
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
            &serde_json::json!({"command": HOLD}),
        ))),
        Reply::Sse(sse(&text_turn("first done", 0))),
        Reply::Sse(sse(&text_turn("second done", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--json", "go"], "").await;
    session.wait_for("call_started").await;
    let answer = session.ctl(&["later", "one more turn"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    session.release();

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
    let result = run.one_event("call_settled")["result"]
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

/// A stopped call keeps its partial work reachable: the model gets the byte and
/// line counts, the output file and its tail (M1 review N2).
#[tokio::test]
async fn a_stopped_call_reports_its_partial_output() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_talk",
            "bash",
            &serde_json::json!({"command": "printf 'before the stop\\n'; sleep 30"}),
        ))),
        Reply::Sse(sse(&text_turn("stopped, I saw what it printed", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("call_started").await;
    let out_path = wait_for_output(&session, "before the stop").await;

    let answer = session
        .ctl(&["stop", "--call", "call_talk", "wrong approach"])
        .await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "done", "the turn continues");

    let log = session.log();
    let settled = of_type(&log, "call_settled")[0];
    assert_eq!(settled["outcome"], "stopped");
    let result = settled["result"].as_str().unwrap();
    assert!(
        result.starts_with("stopped by user: wrong approach"),
        "{result}"
    );
    assert!(result.contains("partial output:"), "{result}");
    assert!(result.contains("before the stop"), "{result}");
    assert_eq!(settled["out_path"].as_str(), Some(out_path.as_str()));
    assert!(settled["bytes"].as_u64().unwrap() > 0, "{settled}");
    assert!(settled["lines"].as_u64().unwrap() >= 1, "{settled}");

    // The model really sees it, in the next request.
    let requests = mock.requests();
    let tool_message = requests[1]
        .messages()
        .iter()
        .find(|message| message["role"] == "tool")
        .expect("a tool result in the next request");
    let content = tool_message["content"].as_str().unwrap();
    assert!(content.contains(&out_path), "{content}");
    assert!(content.contains("before the stop"), "{content}");

    session.end().await;
}

/// A model the provider rejects fails the turn, not the session: the next verb
/// still gets an answer and a good model can be set again (M1 review N6).
#[tokio::test]
async fn a_rejected_model_fails_the_turn_but_not_a_keep_session() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("first answer", 0))),
        Reply::Http(400, "unknown model".to_string()),
        Reply::Sse(sse(&text_turn("back on track", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;

    assert_eq!(session.ctl(&["model", "typo/none"]).await.code, 0);
    let answer = session.ctl(&["say", "try again"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "error");
    let log = session.log();
    assert!(
        log.iter().any(|event| event["type"] == "error"
            && event["where"] == "provider"
            && event["retryable"] == false),
        "the provider refusal must be in the log: {log:#?}"
    );
    assert_eq!(
        of_type(&log, "session_ended").len(),
        0,
        "a failed turn must not end a --keep session"
    );

    // Fix the model and carry on in the same process.
    assert_eq!(session.ctl(&["model", "test/model"]).await.code, 0);
    let answer = session.ctl(&["say", "try once more"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "done");
    let (code, stderr) = session.end().await;
    assert_eq!(code, 0, "{stderr}");
}

/// A `stop` sent during the provider retry wait is obeyed at once instead of
/// waiting it out (M1 review N5).
///
/// Event-driven, not timing-driven (STEER-5 review F1): the wait the session
/// enters is 45 seconds long (`retry_base_ms = retry_cap_ms = 45000`, a budget
/// that affords it), and the test only sends `stop` after reading the `error`
/// event that announces it. So the `ctl` process has seconds to start even on a
/// loaded box, where the suite's default 200/400 ms schedule could finish all
/// three attempts before `ctl` was scheduled — the turn then ended `paused`
/// (the truthful outcome) and the assertion below failed on a test race, not on
/// a product bug. The ceiling stays generous for the same reason.
#[tokio::test]
async fn stop_during_a_provider_backoff_is_obeyed_at_once() {
    let mock = Mock::start(vec![
        Reply::Http(500, "boom".to_string()),
        Reply::Http(500, "boom".to_string()),
        Reply::Http(500, "boom".to_string()),
    ])
    .await;
    let config = "retry_base_ms = 45000\nretry_cap_ms = 45000\nretry_max_attempts = 3\n\
                  retry_max_wait = 180\n";
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], config).await;
    let error = session.wait_for("error").await;
    assert_eq!(error["retryable"], true, "{error}");
    // The wait is announced before it is entered, so this is the marker that
    // says the session is (about to be) waiting rather than retrying.
    assert!(
        error["retry_in_ms"].as_u64().is_some(),
        "the failure announced no wait to stop: {error}"
    );

    let started = Instant::now();
    let answer = session.ctl(&["stop", "no point retrying"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let ended = session.wait_for("turn_ended").await;
    let waited = started.elapsed();
    assert_eq!(ended["outcome"], "stopped", "{ended}");
    assert!(
        waited < Duration::from_secs(15),
        "the stop waited {waited:?} for the retries"
    );
    // The wait was cut, not sat out: nothing was re-sent after the 500.
    assert_eq!(
        mock.request_count(),
        1,
        "the stop was only read after a retry went out"
    );

    session.end().await;
}

/// A `stop` that arrives while the session is idle has nothing to stop: it is
/// dropped, the answer says `dropped` rather than `now`, and it must not stop
/// the next turn (M2 review N1).
#[tokio::test]
async fn a_stop_in_idle_is_answered_as_dropped() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("first", 0))),
        Reply::Sse(sse(&text_turn("second", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;

    let answer = session.ctl(&["stop"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let response = answer.response();
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["applies"], "dropped", "{response}");
    let verb = of_type(&session.log(), "verb")[0].clone();
    assert_eq!(verb["name"], "stop");
    assert_eq!(verb["boundary"], "dropped", "{verb}");

    // The dropped stop did not arm anything: the next message runs a turn.
    let answer = session.ctl(&["say", "keep going"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "done");
    assert_eq!(mock.request_count(), 2);

    session.end().await;
}
