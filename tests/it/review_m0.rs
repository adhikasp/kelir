//! Reviewer-added adversarial tests for M0 (see docs/reviews/M0-review.md).
//!
//! These try to break the loop rather than mirror the implementation. Each test
//! names the finding it proves; a failing test here is a real bug.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use std::time::{Duration, Instant};

use crate::support::*;
use serde_json::{Value, json};

/// F1: two parallel `sleep`s must overlap in wall-clock time. The existing
/// ordering test cannot tell concurrent from sequential execution.
#[tokio::test]
async fn parallel_calls_really_overlap() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&two_tool_turn(
            ("c1", "bash", json!({"command": "sleep 2; echo one"})),
            ("c2", "bash", json!({"command": "sleep 2; echo two"})),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let started = Instant::now();
    let run = run_task(&mock, &["run", "--json", "go"], &[], None, "").await;
    let wall = started.elapsed();
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert!(
        wall < Duration::from_millis(3800),
        "two 2 s sleeps took {wall:?}: calls are not concurrent"
    );
}

/// F2: a background child that inherits stdout must not hold the call open.
/// `sleep 6 &` returns in milliseconds in any shell; kelir waits for pipe EOF.
#[tokio::test]
async fn background_child_does_not_block_the_call() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "c1",
            "bash",
            &json!({"command": "sleep 6 & echo started"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let started = Instant::now();
    let run = run_task(&mock, &["run", "--json", "go"], &[], None, "").await;
    let wall = started.elapsed();
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert!(
        wall < Duration::from_secs(4),
        "`cmd &` blocked the call for {wall:?} (pipe held open by the background child)"
    );
}

/// F3: a provider-supplied call id is used as a directory name. It must not be
/// able to write outside `commands/`.
#[tokio::test]
async fn call_id_cannot_escape_the_commands_dir() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "../../escaped",
            "bash",
            &json!({"command": "echo pwned"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "go"], &[], None, "").await;
    let escaped = run.session_dir.join("commands").join("../../escaped");
    assert!(
        !escaped.exists(),
        "call id `../../escaped` wrote outside the session's commands/ dir"
    );
}

/// F4: ids that repeat across steps (some providers send `call_0` every step,
/// and kelir's own fallback is `call_<position>`) must not overwrite an earlier
/// call's full output on disk.
#[tokio::test]
async fn repeated_call_ids_keep_every_output_file() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_0",
            "bash",
            &json!({"command": "echo first"}),
        ))),
        Reply::Sse(sse(&tool_turn(
            "call_0",
            "bash",
            &json!({"command": "echo second"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "go"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let settled = run.events_of("tool_settled");
    assert_eq!(settled.len(), 2);
    let first_path = settled[0]["out_path"].as_str().unwrap();
    let on_disk = std::fs::read_to_string(first_path).unwrap();
    assert_eq!(
        on_disk, "first\n",
        "the first call's out file ({first_path}) was overwritten by the second"
    );
}

/// F5: a file with mixed line endings must still be editable with multi-line
/// `old` text copied from it.
#[tokio::test]
async fn edit_works_on_mixed_line_endings() {
    // The fixture must exist before the run: seed it through a first bash call.
    let mock_seed = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "s1",
            "bash",
            &json!({"command": "printf 'a\\r\\nb\\nc\\n' > mixed.txt"}),
        ))),
        Reply::Sse(sse(&tool_turn(
            "e1",
            "edit",
            &json!({"path": "mixed.txt", "old": "b\nc", "new": "B\nC"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let run = run_task(&mock_seed, &["run", "--json", "go"], &[], None, "").await;
    let settled = run.events_of("tool_settled");
    assert_eq!(
        settled[1]["outcome"], "ok",
        "edit result: {}",
        settled[1]["result"]
    );
}

/// F6: a call whose arguments are not valid JSON must come back as an error the
/// model can read, not hang or crash the session.
#[tokio::test]
async fn malformed_tool_arguments_become_an_error_result() {
    let mut chunks = tool_call_chunks("bad1", "bash", &json!({}));
    // Replace the arguments with truncated JSON.
    let text = serde_json::to_string(&chunks)
        .unwrap()
        .replace("{}", "{\\\"command\\\": \\\"ec");
    chunks = serde_json::from_str::<Vec<Value>>(&text).unwrap();
    chunks.push(finish("tool_calls"));
    let mock = Mock::start(vec![
        Reply::Sse(sse(&chunks)),
        Reply::Sse(sse(&text_turn("recovered", 0))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "go"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let settled = run.one_event("tool_settled");
    assert_eq!(settled["outcome"], "error");
}

/// F7: non-UTF-8 command output must not break the log or the next request.
#[tokio::test]
async fn non_utf8_output_survives_the_round_trip() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "n1",
            "bash",
            &json!({"command": "printf 'ok\\377\\376end'"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "go"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let settled = run.one_event("tool_settled");
    assert_eq!(settled["bytes"], 7);
    assert!(settled["result"].as_str().unwrap().contains("end"));
}

/// F8: an empty-stdin run with no task is a usage error (exit 2) and must not
/// leave a session half-started that claims success.
#[tokio::test]
async fn empty_stdin_without_a_task_is_exit_2() {
    let mock = Mock::start(vec![]).await;
    let run = run_task(&mock, &["run", "--json"], &[], Some(""), "").await;
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    assert_eq!(mock.request_count(), 0);
}
