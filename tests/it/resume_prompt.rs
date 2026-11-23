//! Resume and compaction prompt tests.
#![allow(clippy::pedantic, clippy::nursery)]

use super::resume_crash::{all_contents, events_of};
use crate::support::*;
use serde_json::json;

#[tokio::test]
async fn the_compact_tool_is_queued_and_applies_at_the_next_step() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_c",
            "compact",
            &json!({"focus": "the api shape"}),
        ))),
        Reply::Sse(sse(&text_turn("SUMMARY: wrote the parser", 0))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let sandbox = Sandbox::new("compact_keep = 0\n");
    let run = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "the task"], &[])
        .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let id = sandbox.only_session();
    let log = sandbox.log(&id);
    let compacted = events_of(&log, "compacted");
    assert_eq!(compacted.len(), 1, "{log:?}");
    assert_eq!(compacted[0]["by"], "agent");
    assert!(compacted[0]["through_seq"].as_u64().unwrap() > 0);
    // The call itself answered that the compaction is queued.
    let settled = events_of(&log, "call_settled");
    assert_eq!(settled.len(), 1);
    assert_eq!(settled[0]["outcome"], "ok");
    assert!(
        settled[0]["result"]
            .as_str()
            .unwrap()
            .contains("compaction queued"),
        "{}",
        settled[0]["result"]
    );

    let requests = mock.requests();
    assert_requests_snapshot("compaction", &requests);
    assert_eq!(requests.len(), 3);
    assert!(requests[1].body.get("tools").is_none());
    assert!(
        requests[1].messages()[1]["content"]
            .as_str()
            .unwrap()
            .contains("Focus for this summary: the api shape")
    );
    let contents = all_contents(&requests[2]);
    assert!(
        contents.iter().any(|text| text == "the task"),
        "{contents:?}"
    );
    assert!(
        contents
            .iter()
            .any(|text| text.contains("SUMMARY: wrote the parser")),
        "{contents:?}"
    );
    assert!(
        !contents
            .iter()
            .any(|text| text.contains("compaction queued")),
        "the compact call should be inside the compacted range: {contents:?}"
    );
}

#[tokio::test]
async fn the_auto_trigger_compacts_at_eighty_percent_of_the_window() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_1",
            "bash",
            &json!({"command": "printf 'ran\\n'"}),
        ))),
        Reply::Sse(sse(&text_turn("SUMMARY: ran one command", 0))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    // The window is 100 tokens and the mock reports 100 prompt tokens after the
    // first step: the next boundary is past 80 %.
    let sandbox = Sandbox::new("window = 100\ncompact_keep = 0\n");
    let run = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "the task"], &[])
        .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let id = sandbox.only_session();
    let log = sandbox.log(&id);
    let compacted = events_of(&log, "compacted");
    assert_eq!(compacted.len(), 1, "{log:?}");
    assert_eq!(compacted[0]["by"], "auto");
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        all_contents(&requests[2])
            .iter()
            .any(|text| text.contains("SUMMARY: ran one command")),
        "the summary is not in the request after the auto compaction"
    );
}

/// N1: an automatic compaction that cannot shrink the context is not worth a
/// provider call. Here the cut only reaches the task, which is pinned anyway:
/// the summary could only add. The trigger declines, the log says so, and the
/// session finishes without a `compacted` event.
#[tokio::test]
async fn a_compaction_that_cannot_shrink_is_skipped() {
    let bash = |id: &str, n: u32| {
        Reply::Sse(sse(&tool_turn(
            id,
            "bash",
            &json!({"command": format!("printf 'step{n}\\n'")}),
        )))
    };
    let mock = Mock::start(vec![
        bash("call_1", 1),
        bash("call_2", 2),
        Reply::Sse(sse(&text_turn("finished", 0))),
    ])
    .await;
    // The window makes the trigger fire after the first step; `compact_keep`
    // is the default 4, which here leaves the task as the whole range.
    let sandbox = Sandbox::new("window = 100\n");
    let run = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "the task"], &[])
        .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let log = sandbox.log(&sandbox.only_session());
    assert!(
        events_of(&log, "compacted").is_empty(),
        "a compaction that grows the context was recorded: {log:?}"
    );
    let skipped = events_of(&log, "error")
        .into_iter()
        .find(|event| event["where"] == "compact")
        .unwrap_or_else(|| panic!("the skip is not in the log: {log:?}"));
    assert!(
        skipped["message"]
            .as_str()
            .unwrap()
            .contains("would not be smaller"),
        "{skipped}"
    );
    // No summarising request was paid for: every request carried the tool list.
    assert!(
        mock.requests()
            .iter()
            .all(|request| request.body.get("tools").is_some()),
        "a summary was requested for a compaction that could not shrink"
    );
}

/// N2: an agent-called `compact` is only a queued request in memory. A kill
/// between the tool's `compaction queued` result and the boundary that would
/// summarise must not lose it: the resume re-queues it.
#[tokio::test]
async fn a_queued_compaction_survives_a_crash_and_is_applied_on_resume() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_c",
            "compact",
            &json!({"focus": "the parser"}),
        ))),
        Reply::Sse(sse(&text_turn("SUMMARY: wrote the parser", 0))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let sandbox = Sandbox::new("compact_keep = 0\n");
    let first = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "the task"],
            &[("KELIR_TEST_CRASH_AT", "tool_settled")],
        )
        .await;
    assert_ne!(first.code, 0);
    assert!(
        first.stderr.contains("KELIR_TEST_CRASH_AT="),
        "stderr: {}",
        first.stderr
    );
    let id = sandbox.only_session();
    let second = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "--resume", id.as_str()],
            &[],
        )
        .await;
    assert_eq!(second.code, 0, "stderr: {}", second.stderr);

    let log = sandbox.log(&id);
    let compacted = events_of(&log, "compacted");
    assert_eq!(
        compacted.len(),
        1,
        "the queued compaction was lost: {log:?}"
    );
    assert_eq!(compacted[0]["by"], "agent");
    // N4: the summarising request's own tokens are in the log, marked so a
    // reader can tell it is not a step of the turn.
    let summary_usage = events_of(&log, "step_usage")
        .into_iter()
        .find(|event| event["kind"] == "compact")
        .unwrap_or_else(|| panic!("the summary's usage is not logged: {log:?}"));
    assert_eq!(summary_usage["prompt_tokens"], 200);
    let requests = mock.requests();
    let summarising = requests
        .iter()
        .find(|request| request.body.get("tools").is_none())
        .expect("no summarising request after the resume");
    assert!(
        summarising.messages()[1]["content"]
            .as_str()
            .unwrap()
            .contains("Focus for this summary: the parser"),
        "the focus was lost"
    );
}

#[tokio::test]
async fn a_compaction_asked_for_while_a_tool_runs_waits_for_the_boundary() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_1",
            "bash",
            &json!({"command": "sleep 1; printf 'ran\\n'"}),
        ))),
        Reply::Sse(sse(&text_turn("SUMMARY: one long command", 0))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let mut live = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "the task"],
        "compact_keep = 0\n",
    )
    .await;
    live.wait_for("call_started").await;
    let answer = live.ctl(&["compact"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    live.wait_for("call_settled").await;
    live.wait_for("compacted").await;
    live.wait_for("turn_ended").await;
    let log = live.log();
    live.end().await;

    let index = |event_type: &str| {
        log.iter()
            .position(|event| event["type"] == event_type)
            .unwrap_or_else(|| panic!("no {event_type} in {log:?}"))
    };
    assert!(
        index("call_started") < index("verb"),
        "the verb was not taken while the tool ran"
    );
    assert!(
        index("verb") < index("call_settled"),
        "the verb was taken between the call and its result"
    );
    assert!(
        index("call_settled") < index("compacted"),
        "the compaction landed before the batch settled: {log:?}"
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 3, "{requests:?}");
    assert!(requests[1].body.get("tools").is_none());
}
