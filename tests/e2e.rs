//! End-to-end: the real `kelir` binary against a scripted mock provider.
//!
//! Assertions are on the event log, the NDJSON stdout, the request bodies and
//! headers the mock received, and on files the tools left behind.

mod support;

use std::path::Path;

use serde_json::{Value, json};
use support::*;

fn heads_back_path(value: &Value, tail: &[&str]) -> bool {
    let path = Path::new(value.as_str().unwrap_or(""));
    let mut expected = std::path::PathBuf::new();
    for part in tail {
        expected.push(part);
    }
    path.ends_with(expected)
}

#[tokio::test]
async fn plain_answer_streams_events_and_usage() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("hi there", 96)))]).await;
    let run = run_task(&mock, &["run", "--json", "say hi"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let types = run.event_types();
    assert_eq!(
        &types[..3],
        &["session_started", "user", "step_started"],
        "{types:?}"
    );
    assert_eq!(types[3], "assistant");
    assert_eq!(types[4], "usage");
    assert_eq!(&types[types.len() - 2..], &["turn_ended", "session_ended"]);

    // Monotonic seq from 1, one per line, with RFC 3339 timestamps.
    let seqs: Vec<u64> = run
        .events
        .iter()
        .map(|event| event["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
    for event in &run.events {
        assert!(event["ts"].as_str().unwrap().ends_with('Z'));
    }

    let assistant = run.one_event("assistant");
    assert_eq!(assistant["text"], "hi there");
    assert_eq!(assistant["finish_reason"], "stop");
    assert_eq!(assistant["step"], 1);

    let usage = run.one_event("usage");
    assert_eq!(usage["prompt_tokens"], 200);
    assert_eq!(usage["completion_tokens"], 8);
    assert_eq!(usage["cached_tokens"], 96);
    assert_eq!(usage["cache_write_tokens"], 40);
    assert!(usage["cost"].as_f64().unwrap() > 0.0);

    assert_eq!(run.one_event("turn_ended")["outcome"], "done");
    assert_eq!(run.one_event("session_ended")["reason"], "done");

    let started = run.one_event("session_started");
    assert_eq!(started["provider"], "openrouter");
    assert_eq!(started["model"], "test/model");
    assert_eq!(started["depth"], 0);
    assert_eq!(started["tools"], json!(["bash", "edit"]));
    assert!(started["system"]["hash"].as_str().unwrap().len() == 16);
    assert!(
        started["system"]["text"]
            .as_str()
            .unwrap()
            .contains("kelir")
    );

    // NDJSON stdout mirrors the log line for line, then a result event.
    let lines = run.json_lines();
    let event_lines: Vec<&Value> = lines
        .iter()
        .filter(|line| line["type"] != "result")
        .collect();
    assert_eq!(event_lines.len(), run.events.len());
    for (line, event) in event_lines.iter().zip(&run.events) {
        assert_eq!(*line, event);
    }
    let result = run.result();
    assert_eq!(result["outcome"], "done");
    assert_eq!(result["final_text"], "hi there");
    assert_eq!(result["exit_code"], 0);
    assert_eq!(result["steps"], 1);

    // One request; the session id is in the body, cache_control is not (this
    // model caches automatically), and nothing sets provider.order.
    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/chat/completions");
    assert_eq!(request.body["model"], "test/model");
    assert_eq!(request.body["stream"], true);
    assert_eq!(request.body["stream_options"]["include_usage"], true);
    assert!(request.body.get("provider").is_none());
    assert!(request.body.get("cache_control").is_none());
    assert_eq!(request.body["session_id"], started["id"]);
    assert_eq!(request.body["tools"].as_array().unwrap().len(), 2);
    assert_eq!(request.body["messages"][0]["role"], "system");
    assert_eq!(request.body["messages"][1]["role"], "user");
    assert_eq!(request.body["messages"][1]["content"], "say hi");
}

#[tokio::test]
async fn task_can_arrive_on_stdin() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let run = run_task(&mock, &["run", "--json"], &[], Some("do the thing\n"), "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(run.one_event("user")["text"], "do the thing");
    assert_eq!(run.one_event("user")["delivery"], "now");
}

#[tokio::test]
async fn one_bash_call_then_answer() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_1",
            "bash",
            &json!({"command": "printf 'kelir-ok\\n'"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 120))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "run one command"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let started = run.one_event("tool_started");
    assert_eq!(started["call_id"], "call_1");
    assert_eq!(started["name"], "bash");
    assert_eq!(started["input"]["command"], "printf 'kelir-ok\\n'");

    let settled = run.one_event("tool_settled");
    assert_eq!(settled["call_id"], "call_1");
    assert_eq!(settled["outcome"], "ok");
    assert_eq!(settled["exit_code"], 0);
    assert_eq!(settled["result"], "kelir-ok\n");
    assert_eq!(settled["spilled"], false);
    assert_eq!(settled["bytes"], 9);
    assert!(heads_back_path(
        &settled["out_path"],
        &["commands", "call_1", "out"]
    ));
    assert_eq!(run.command_out("call_1"), "kelir-ok\n");

    assert_eq!(run.events_of("step_started").len(), 2);
    assert_eq!(run.events_of("assistant").len(), 2);

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].roles(),
        vec!["system", "user", "assistant", "tool"]
    );
    assert_eq!(requests[1].messages()[2]["tool_calls"][0]["id"], "call_1");
    assert_eq!(
        requests[1].messages()[2]["tool_calls"][0]["function"]["name"],
        "bash"
    );
    assert_eq!(requests[1].messages()[3]["tool_call_id"], "call_1");
    assert_eq!(requests[1].messages()[3]["content"], "kelir-ok\n");

    // The prefix of step 2 begins byte-identically with step 1's messages.
    let first = requests[0].body["messages"].as_array().unwrap();
    let second = requests[1].body["messages"].as_array().unwrap();
    assert_eq!(&second[..first.len()], first.as_slice());
    assert_eq!(requests[0].body["tools"], requests[1].body["tools"]);

    assert_eq!(run.one_event("turn_ended")["outcome"], "done");
}

#[tokio::test]
async fn parallel_calls_settle_in_call_order() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&two_tool_turn(
            (
                "call_slow",
                "bash",
                json!({"command": "sleep 0.6; printf 'slow\\n'"}),
            ),
            ("call_fast", "bash", json!({"command": "printf 'fast\\n'"})),
        ))),
        Reply::Sse(sse(&text_turn("both done", 200))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "run two"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let started: Vec<&Value> = run.events_of("tool_started");
    assert_eq!(started.len(), 2);
    assert_eq!(started[0]["call_id"], "call_slow");
    assert_eq!(started[1]["call_id"], "call_fast");

    let settled: Vec<&Value> = run.events_of("tool_settled");
    assert_eq!(settled.len(), 2);
    assert_eq!(settled[0]["call_id"], "call_slow");
    assert_eq!(settled[1]["call_id"], "call_fast");
    assert_eq!(settled[0]["result"], "slow\n");
    assert_eq!(settled[1]["result"], "fast\n");

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let tool_ids: Vec<&str> = requests[1]
        .messages()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| message["tool_call_id"].as_str().unwrap())
        .collect();
    assert_eq!(tool_ids, vec!["call_slow", "call_fast"]);
}

#[tokio::test]
async fn edit_creates_a_file_then_bash_reads_it() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_edit",
            "edit",
            &json!({"path": "note.txt", "old": "", "new": "hello kelir\n"}),
        ))),
        Reply::Sse(sse(&tool_turn(
            "call_cat",
            "bash",
            &json!({"command": "cat note.txt"}),
        ))),
        Reply::Sse(sse(&text_turn("the note says hello kelir", 300))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "create a note"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    assert_eq!(
        std::fs::read_to_string(run.cwd.join("note.txt")).unwrap(),
        "hello kelir\n"
    );
    let settled: Vec<&Value> = run.events_of("tool_settled");
    assert_eq!(settled.len(), 2);
    assert_eq!(settled[0]["outcome"], "ok");
    assert!(
        settled[0]["result"]
            .as_str()
            .unwrap()
            .contains("created note.txt")
    );
    assert_eq!(settled[1]["result"], "hello kelir\n");
    assert_eq!(run.events_of("step_started").len(), 3);
}

#[tokio::test]
async fn big_output_spills_to_a_notice() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_big",
            "bash",
            &json!({"command": "yes x | head -c 20000"}),
        ))),
        Reply::Sse(sse(&text_turn("ok", 10))),
    ])
    .await;
    let run = run_task(
        &mock,
        &["run", "--json", "make a lot of output"],
        &[],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let settled = run.one_event("tool_settled");
    assert_eq!(settled["outcome"], "ok");
    assert_eq!(settled["spilled"], true);
    assert_eq!(settled["bytes"], 20_000);

    let result = settled["result"].as_str().unwrap();
    assert!(result.contains("output spilled"), "{result}");
    assert!(result.contains("20000 bytes"), "{result}");
    assert!(result.contains("first 1024 bytes"), "{result}");
    assert!(result.contains("last 1024 bytes"), "{result}");
    assert!(result.contains("hint: rg -n"), "{result}");
    assert!(!result.contains(&"x".repeat(5_000)));

    let out = std::fs::read(run.session_dir.join("commands/call_big/out")).unwrap();
    assert_eq!(out.len(), 20_000);
}

#[tokio::test]
async fn http_500_is_retried_with_a_retryable_error() {
    let mock = Mock::start(vec![
        Reply::Http(500, "boom".into()),
        Reply::Sse(sse(&text_turn("recovered", 0))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "try"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let errors: Vec<&Value> = run.events_of("error");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["retryable"], true);
    assert_eq!(errors[0]["where"], "provider");
    assert!(errors[0]["message"].as_str().unwrap().contains("500"));
    assert_eq!(mock.request_count(), 2);
    assert_eq!(run.result()["final_text"], "recovered");
    assert_eq!(run.one_event("turn_ended")["outcome"], "done");
}

#[tokio::test]
async fn mid_stream_disconnect_is_retried() {
    let partial = format!("data: {}\n\n", delta("half of it"));
    let mock = Mock::start(vec![
        Reply::Disconnect(partial),
        Reply::Sse(sse(&text_turn("whole", 0))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "try"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let errors: Vec<&Value> = run.events_of("error");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["retryable"], true);
    assert_eq!(mock.request_count(), 2);
    // The half-turn was never committed: exactly one assistant event.
    assert_eq!(run.events_of("assistant").len(), 1);
    assert_eq!(run.one_event("assistant")["text"], "whole");
    assert_eq!(run.result()["outcome"], "done");
}

#[tokio::test]
async fn exhausted_retries_end_the_turn_with_exit_1() {
    let mock = Mock::start(vec![
        Reply::Http(500, "down".into()),
        Reply::Http(500, "down".into()),
        Reply::Http(500, "down".into()),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "try"], &[], None, "").await;
    assert_eq!(run.code, 1, "stderr: {}", run.stderr);
    assert_eq!(mock.request_count(), 3);
    assert_eq!(run.events_of("error").len(), 3);
    assert_eq!(run.one_event("turn_ended")["outcome"], "error");
    assert_eq!(run.one_event("session_ended")["reason"], "error");
    assert_eq!(run.result()["outcome"], "error");
    assert_eq!(run.result()["exit_code"], 1);
}

#[tokio::test]
async fn openrouter_headers_and_cache_control() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let run = run_task(
        &mock,
        &[
            "run",
            "--json",
            "--model",
            "anthropic/claude-sonnet-4.5",
            "hi",
        ],
        &[],
        None,
        "cache_ttl = \"1h\"\n",
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let request = &mock.requests()[0];
    assert_eq!(request.path, "/v1/chat/completions");
    assert!(request.header("x-opencode-session").is_none());
    assert!(
        request
            .header("authorization")
            .unwrap()
            .starts_with("Bearer ")
    );
    let session_id = run.one_event("session_started")["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(request.body["session_id"], session_id);
    assert_eq!(
        request.body["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
    assert!(request.body.get("provider").is_none());
    assert_eq!(
        run.one_event("step_started")["model"],
        "anthropic/claude-sonnet-4.5"
    );
}

#[tokio::test]
async fn go_headers_carry_the_session_id_and_kelir_user_agent() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let run = run_task(
        &mock,
        &["run", "--json", "--provider", "opencode-go", "hi"],
        &[],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let request = &mock.requests()[0];
    let session_id = run.one_event("session_started")["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        request.header("x-opencode-session"),
        Some(session_id.as_str())
    );
    assert!(
        request.header("user-agent").unwrap().starts_with("kelir/"),
        "{:?}",
        request.header("user-agent")
    );
    assert!(request.body.get("session_id").is_none());
    assert!(request.body.get("cache_control").is_none());
}

#[tokio::test]
async fn unknown_provider_is_a_usage_error() {
    let mock = Mock::start(vec![]).await;
    let run = run_task(
        &mock,
        &["run", "--json", "--provider", "nope", "hi"],
        &[],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    assert!(run.stderr.contains("unknown provider"), "{}", run.stderr);
    assert_eq!(mock.request_count(), 0);
    assert!(run.events.is_empty());
}

#[tokio::test]
async fn a_missing_key_is_a_usage_error() {
    let mock = Mock::start(vec![]).await;
    let run = run_task(
        &mock,
        &["run", "--json", "hi"],
        &[("OPENROUTER_API_KEY", "")],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    assert!(run.stderr.contains("missing API key"), "{}", run.stderr);
    assert_eq!(mock.request_count(), 0);
}
