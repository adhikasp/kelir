//! M3 end-to-end: durability, resume and compaction against the mock provider.
//!
//! The crash tests abort the real binary (SIGABRT / a Windows non-zero exit)
//! through the documented `KELIR_TEST_CRASH_AT` hook, then resume the same log and
//! assert on the log, the requests the mock received and a side-effect counter:
//! a completed command is never re-run, and a call that may or may not have run
//! is reported as unknown.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use std::sync::Arc;

use crate::support::*;
use serde_json::{Value, json};

pub(super) fn events_of<'a>(log: &'a [Value], event_type: &str) -> Vec<&'a Value> {
    log.iter()
        .filter(|event| event["type"] == event_type)
        .collect()
}

fn content(event: &Value) -> String {
    event["text"].as_str().unwrap_or("").to_string()
}

fn tool_contents(request: &Recorded) -> Vec<String> {
    request
        .messages()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| message["content"].as_str().unwrap_or("").to_string())
        .collect()
}

pub(super) fn all_contents(request: &Recorded) -> Vec<String> {
    request
        .messages()
        .iter()
        .map(|message| message["content"].as_str().unwrap_or("").to_string())
        .collect()
}

/// One crash point: abort the run, resume it, prove what happened.
///
/// The command counts its own runs; after the resume the count must not grow
/// (never twice), and it is 1 only where the command had really run.
#[tokio::test]
async fn a_crash_at_any_of_five_points_resumes_without_re_running_a_command() {
    for point in [
        "tool_started",
        "mid_tool",
        "tool_settled",
        "mid_stream",
        "usage",
    ] {
        crash_point(point).await;
    }
}

async fn crash_point(point: &str) {
    // The command counts its own runs; `sleep` keeps it in flight for
    // `mid_tool`.
    let command = "printf 'run\\n' >> side_effect.log; sleep 1";
    let script = if point == "mid_stream" {
        vec![
            Reply::Sse(sse(&text_turn("half an answer", 0))),
            Reply::Sse(sse(&text_turn("the whole answer", 0))),
        ]
    } else {
        vec![
            Reply::Sse(sse(&tool_turn(
                "call_1",
                "bash",
                &json!({"command": command}),
            ))),
            Reply::Sse(sse(&text_turn("finished", 0))),
        ]
    };
    let mock = Mock::start(script).await;
    let sandbox = Sandbox::new("");

    let first = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "the task"],
            &[("KELIR_TEST_CRASH_AT", point)],
        )
        .await;
    assert_ne!(first.code, 0, "{point}: the process exited cleanly");
    assert!(
        first.stderr.contains("KELIR_TEST_CRASH_AT="),
        "{point}: the crash hook did not fire; stderr: {}",
        first.stderr
    );
    let id = sandbox.only_session();
    let before = sandbox.log(&id);
    assert!(
        !before.iter().any(|event| event["type"] == "session_ended"),
        "{point}: the log says the session ended cleanly: {before:?}"
    );
    assert!(
        !before.iter().any(|event| event["type"] == "turn_ended"),
        "{point}: the log says the turn ended cleanly"
    );

    // How many times the command had run when the session died. Since M4 an
    // unpromoted call is `kelir bg-shim` -> shell -> command, and its tree dies
    // with the session, so a crash 150 ms after dispatch can land before the
    // command's first write on a loaded box: the outcome really is unknown, and
    // 0 and 1 are both honest at `mid_tool`. The tree is given time to be gone
    // first (a Unix shim notices its dead owner within one 25 ms poll), so the
    // number cannot move while the resume runs.
    if point == "mid_tool" {
        sandbox.trees_gone(&id).await;
    }
    let at_crash = sandbox.lines_in("side_effect.log");

    let second = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "--resume", id.as_str()],
            &[],
        )
        .await;
    assert_eq!(
        second.code, 0,
        "{point}: resume failed; stdout: {}\nstderr: {}",
        second.stdout, second.stderr
    );

    let log = sandbox.log(&id);
    let expect_unsettled = matches!(point, "tool_started" | "mid_tool" | "usage");
    // A started call nobody settled is reported as unknown, and the model sees
    // that in the next request; a call that completed keeps its real result.
    if expect_unsettled {
        let unknown = log
            .iter()
            .find(|event| event["type"] == "call_settled" && event["outcome"] == "unknown")
            .unwrap_or_else(|| panic!("{point}: no unknown outcome: {log:?}"));
        assert_eq!(unknown["id"], "call_1");
        assert!(
            unknown["result"]
                .as_str()
                .unwrap()
                .contains("unknown outcome"),
            "{point}: {}",
            unknown["result"]
        );
    }
    let requests = mock.requests();
    let last = requests.last().expect("the resumed request");
    let tool_texts = tool_contents(last);
    if expect_unsettled {
        assert!(
            tool_texts
                .iter()
                .any(|text| text.contains("unknown outcome")),
            "{point}: the model was not told the outcome is unknown: {tool_texts:?}"
        );
    } else if point == "tool_settled" {
        // The completed call keeps its real result: no unknown, no re-run.
        assert!(
            tool_texts
                .iter()
                .all(|text| !text.contains("unknown outcome")),
            "{point}: a completed call was reported as unknown: {tool_texts:?}"
        );
        let settled = log
            .iter()
            .find(|event| event["type"] == "call_settled")
            .expect("the settled call");
        assert_eq!(settled["outcome"], "ok", "{point}: {settled}");
    }

    // The counter proves the command was not run again: a completed command is
    // never re-run, and a resume never adds a run of its own, whether the crash
    // landed before the command's first write or after it.
    let runs = sandbox.lines_in("side_effect.log");
    assert_eq!(
        runs, at_crash,
        "{point}: the resume ran the command again ({at_crash} run(s) at the crash, {runs} after)"
    );
    match point {
        // The command was in flight: it ran at most once, and whether that was
        // 0 or 1 depends on how far its tree got before the crash.
        "mid_tool" => assert!(
            at_crash <= 1,
            "{point}: the command ran {at_crash} time(s), expected at most 1"
        ),
        // The call had completed and was settled in the log: it ran, once.
        "tool_settled" => assert_eq!(at_crash, 1, "{point}: the command ran {at_crash} time(s)"),
        // The crash came before the command could start.
        _ => assert_eq!(at_crash, 0, "{point}: the command ran {at_crash} time(s)"),
    }
    // The log agrees: the call was started once if the session got that far
    // dispatches it again, not even to settle an unknown outcome.
    let expected_starts = if matches!(point, "usage" | "mid_stream") {
        0
    } else {
        1
    };
    assert_eq!(
        events_of(&log, "call_started")
            .iter()
            .filter(|event| event["kind"] == "tool")
            .count(),
        expected_starts,
        "{point}: the call was started again on resume: {log:?}"
    );

    assert_eq!(log.last().unwrap()["type"], "session_ended", "{point}");
    let ended: Vec<&Value> = events_of(&log, "turn_ended");
    assert_eq!(ended.last().unwrap()["outcome"], "done", "{point}");

    if point == "mid_stream" {
        // Nothing from the half-streamed response was committed.
        assert_requests_snapshot("resume_after_crash", &mock.requests());
        let assistants = events_of(&log, "assistant");
        assert_eq!(assistants.len(), 1, "{point}: {assistants:?}");
        assert_eq!(content(assistants[0]), "the whole answer");
    }
}

#[tokio::test]
async fn a_torn_last_line_is_reported_on_resume_and_the_session_continues() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_1",
            "bash",
            &json!({"command": "printf 'done\\n'"}),
        ))),
        Reply::Sse(sse(&text_turn("finished", 0))),
    ])
    .await;
    let sandbox = Sandbox::new("");
    let first = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "the task"],
            &[("KELIR_TEST_CRASH_AT", "usage")],
        )
        .await;
    assert_ne!(first.code, 0);
    let id = sandbox.only_session();

    // A crash in the middle of a write leaves half a line at the end.
    let path = sandbox.session_dir(&id).join("events.jsonl");
    let mut text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.ends_with('\n'),
        "the sample log is not line-terminated"
    );
    text.push_str("{\"seq\": 99, \"ts\": \"x\", \"type\": \"assis");
    std::fs::write(&path, text).unwrap();

    let second = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "--resume", id.as_str()],
            &[],
        )
        .await;
    assert_eq!(second.code, 0, "stderr: {}", second.stderr);
    let log = sandbox.log(&id);
    let errors = events_of(&log, "error");
    assert!(
        errors.iter().any(|event| {
            event["where"] == "log"
                && event["message"]
                    .as_str()
                    .unwrap()
                    .contains("torn last log line")
        }),
        "no log error for the torn line: {log:?}"
    );
    // Reported, skipped, and the session still finished.
    assert_eq!(log.last().unwrap()["type"], "session_ended");
    assert!(events_of(&log, "compacted").is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn resume_replaces_the_stale_socket_of_the_dead_process() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("one", 0))),
        Reply::Sse(sse(&text_turn("two", 0))),
    ])
    .await;
    let sandbox = Sandbox::new("");
    let first = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "the task"],
            &[("KELIR_TEST_CRASH_AT", "mid_stream")],
        )
        .await;
    assert_ne!(first.code, 0);
    let id = sandbox.only_session();
    // With a deep KELIR_STATE_DIR (CI runners), the socket is bound to a
    // short path recorded in `sock.path` instead of the canonical `sock`.
    // The contract is that one of them names the crashed session's socket.
    let session_dir = sandbox.session_dir(&id);
    let sock = session_dir.join("sock");
    let recorded = std::fs::read_to_string(session_dir.join("sock.path")).unwrap_or_default();
    let falls_back = !recorded.trim().is_empty();
    assert!(
        sock.exists() || falls_back,
        "the crashed session left no socket file"
    );
    if falls_back {
        assert!(
            std::path::Path::new(recorded.trim()).exists(),
            "the recorded fallback socket does not exist"
        );
    }

    let second = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "--resume", id.as_str()],
            &[],
        )
        .await;
    assert_eq!(second.code, 0, "stderr: {}", second.stderr);
    let log = sandbox.log(&id);
    assert_eq!(log.last().unwrap()["type"], "session_ended");
}

#[tokio::test]
async fn resume_refuses_a_session_that_is_still_running() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![Reply::Gated(gate.clone(), sse(&text_turn("held", 0)))]).await;
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    live.wait_for("step_started").await;

    let sandbox = Sandbox::from_live(&live);
    let attempt = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "--resume", live.id.as_str()],
            &[],
        )
        .await;
    assert_eq!(attempt.code, 1, "stdout: {}", attempt.stdout);
    assert!(
        attempt.stderr.contains("already running"),
        "stderr: {}",
        attempt.stderr
    );

    // `notify_one` stores a permit when the mock has not yet reached its
    // `notified()` poll: under a loaded runner the request can still be in
    // flight here, and `notify_waiters` would be lost.
    gate.notify_one();
    live.wait_for("turn_ended").await;
    live.end().await;
}

#[tokio::test]
async fn resume_adds_a_new_task_after_a_clean_end() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("first answer", 0))),
        Reply::Sse(sse(&text_turn("second answer", 0))),
    ])
    .await;
    let sandbox = Sandbox::new("");
    let first = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "first task"], &[])
        .await;
    assert_eq!(first.code, 0, "stderr: {}", first.stderr);
    let id = sandbox.only_session();

    let second = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "--resume", id.as_str(), "second question"],
            &[],
        )
        .await;
    assert_eq!(second.code, 0, "stderr: {}", second.stderr);

    let log = sandbox.log(&id);
    let users: Vec<String> = events_of(&log, "user")
        .iter()
        .map(|event| content(event))
        .collect();
    assert_eq!(users, vec!["first task", "second question"]);
    assert_eq!(events_of(&log, "turn_ended").len(), 2);
    assert_eq!(
        events_of(&log, "session_started").len(),
        1,
        "a resume never rewrites the frozen prefix"
    );
    assert_eq!(
        events_of(&log, "session_ended").len(),
        2,
        "the first run ended its own session"
    );

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].roles(),
        vec!["system", "user", "assistant", "user"]
    );
    assert_eq!(requests[1].messages()[3]["content"], "second question");

    // A resume with nothing in flight and nothing new has no turn to run.
    let third = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "--resume", id.as_str()],
            &[],
        )
        .await;
    assert_eq!(third.code, 0, "stderr: {}", third.stderr);
    assert_eq!(mock.request_count(), 2, "the idle resume made a request");
}

#[tokio::test]
async fn a_log_that_was_never_closed_resumes_without_a_new_task() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("one", 0))),
        Reply::Sse(sse(&text_turn("two", 0))),
    ])
    .await;
    let sandbox = Sandbox::new("");
    let first = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "the task"], &[])
        .await;
    assert_eq!(first.code, 0, "stderr: {}", first.stderr);
    let id = sandbox.only_session();

    // What a kill between two turns leaves: the turn ended, the log was never
    // closed. (The crash hooks all fire inside a turn, so the suffix is made by
    // dropping the `session_ended` the killed process never wrote.)
    let path = sandbox.session_dir(&id).join("events.jsonl");
    let kept: String = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|line| !line.contains("\"type\":\"session_ended\""))
        .map(|line| format!("{line}\n"))
        .collect();
    std::fs::write(&path, kept).unwrap();

    let second = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "--resume", id.as_str()],
            &[],
        )
        .await;
    assert_eq!(second.code, 0, "stderr: {}", second.stderr);
    assert_eq!(mock.request_count(), 2, "an unclosed log was not continued");
    // S4: the request may not end on the assistant's final answer (providers
    // read a trailing assistant message as prefill or refuse it), so the resume
    // says what happened in a message of its own.
    let resumed_request = mock.requests().pop().unwrap();
    assert_eq!(
        resumed_request.roles().last(),
        Some(&"user"),
        "the resumed request ends on the assistant's answer"
    );
    assert!(
        resumed_request.messages().last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("interrupted between turns"),
        "{:?}",
        resumed_request.messages().last()
    );
    let log = sandbox.log(&id);
    assert_eq!(events_of(&log, "turn_ended").len(), 2);
    assert_eq!(log.last().unwrap()["type"], "session_ended");
}

// --------------------------------------------------------------- compaction

#[tokio::test]
async fn a_human_compact_keeps_the_task_and_human_messages_verbatim() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn(&"a long answer ".repeat(400), 0))),
        Reply::Sse(sse(&text_turn("SUMMARY: the task was started", 0))),
        Reply::Sse(sse(&text_turn("answer two", 0))),
    ])
    .await;
    let mut live = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "the task"],
        "compact_keep = 0\n",
    )
    .await;
    live.wait_for("turn_ended").await;

    let answer = live.ctl(&["compact", "keep the failing test"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let answer = live.ctl(&["say", "next"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    live.wait_for("turn_ended").await;
    let log = live.log();
    live.end().await;

    let compacted = events_of(&log, "compacted");
    assert_eq!(compacted.len(), 1, "{log:?}");
    assert_eq!(compacted[0]["by"], "user");
    assert_eq!(compacted[0]["cache_restart"], true);
    // With `/compact` immediate, the “next” message arrives *after* the
    // compaction, so only the original task is pinned (compact_keep = 0).
    assert_eq!(compacted[0]["pinned_seqs"].as_array().unwrap().len(), 1);
    assert!(
        compacted[0]["tokens_after"].as_u64().unwrap()
            < compacted[0]["tokens_before"].as_u64().unwrap(),
        "{compacted:?}"
    );

    let requests = mock.requests();
    assert_eq!(requests.len(), 3, "{requests:?}");
    // The summarising request is sealed: no tools, the focus, the transcript.
    assert!(requests[1].body.get("tools").is_none());
    let summarising = requests[1].messages()[1]["content"].as_str().unwrap();
    assert!(
        summarising.contains("Focus for this summary: keep the failing test"),
        "{summarising}"
    );
    assert!(summarising.contains("the task"), "{summarising}");
    // The request after the compaction carries the summary, the task and the
    // human message verbatim, and no longer the old answer.
    let contents = all_contents(&requests[2]);
    assert!(
        contents
            .iter()
            .any(|text| text.contains("SUMMARY: the task was started")),
        "{contents:?}"
    );
    assert!(
        contents.iter().any(|text| text == "the task"),
        "{contents:?}"
    );
    assert!(contents.iter().any(|text| text == "next"), "{contents:?}");
    assert!(
        !contents.iter().any(|text| text.contains("a long answer")),
        "the compacted assistant message is still there: {contents:?}"
    );
    // The system prefix and the tool list are untouched by a compaction.
    assert_eq!(requests[0].body["tools"], requests[2].body["tools"]);
    assert_eq!(
        requests[0].messages()[0]["content"],
        requests[2].messages()[0]["content"]
    );
}
