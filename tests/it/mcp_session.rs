//! MCP tool calls inside live sessions.
#![allow(clippy::pedantic, clippy::nursery)]

use crate::support::*;
use serde_json::json;

use super::mcp_core::*;

#[tokio::test]
async fn a_call_inside_a_bash_call_is_logged_between_its_start_and_settle() {
    let files = Files::new();
    let run = session_run(
        &files,
        &server(""),
        "\"$KELIR_BIN\" mcp demo echo text=from-the-model",
    )
    .await;
    let kinds = types(&run.events);
    let (a, b, c, d) = (
        call_index(&run.events, "call_started", "tool"),
        call_index(&run.events, "call_started", "mcp"),
        call_index(&run.events, "call_settled", "mcp"),
        call_index(&run.events, "call_settled", "tool"),
    );
    assert!(a < b && b < c && c < d, "{kinds:?}");

    let started = run.one_call("call_started", "mcp");
    assert_eq!(started["info"]["name"], "demo/echo");
    assert_eq!(
        started["info"]["input"],
        json!({"text": "<string, 14 bytes>"})
    );
    let settled = run.one_call("call_settled", "mcp");
    assert_eq!(settled["id"], started["id"]);
    assert_eq!(settled["outcome"], "ok");
    assert_eq!(settled["exit_code"], 0);
    let out_path = settled["out_path"].as_str().unwrap();
    assert!(
        out_path.contains("calls"),
        "the result lives in the session dir: {out_path}"
    );
    assert_eq!(std::fs::read_to_string(out_path).unwrap(), "from-the-model");

    let tool = run.one_call("call_settled", "tool");
    assert!(
        payload_text(&tool["result"]).contains("from-the-model"),
        "{tool}"
    );
}

#[tokio::test]
async fn a_big_result_spills_and_still_pipes_whole() {
    let files = Files::new();
    // Piped: the consumer sees every byte, so `| jq` works on big results.
    let run = session_run(&files, &server(""), "\"$KELIR_BIN\" mcp demo big | wc -c").await;
    let tool = run.one_call("call_settled", "tool");
    let counted: u64 = payload_text(&tool["result"])
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("wc -c printed {}", payload_text(&tool["result"])));
    assert!(counted >= 20_000, "{counted}");
    let settled = run.one_call("call_settled", "mcp");
    assert!(settled["bytes"].as_u64().unwrap() >= 20_000, "{settled}");
    let full = std::fs::read_to_string(settled["out_path"].as_str().unwrap()).unwrap();
    assert!(full.len() >= 20_000);

    // Not piped: the bash call's own 10 KB rule gives the model a notice.
    let run = session_run(&files, &server(""), "\"$KELIR_BIN\" mcp demo big").await;
    let tool = run.one_call("call_settled", "tool");
    assert_eq!(tool["spilled"], true, "{tool}");
    assert!(
        payload_text(&tool["result"]).contains("output spilled"),
        "{tool}"
    );
}

#[tokio::test]
async fn the_broker_keeps_one_server_for_the_session_and_it_dies_with_it() {
    let files = Files::new();
    let toml = server("");
    let run = session_run(
        &files,
        &toml,
        "\"$KELIR_BIN\" mcp ls; \"$KELIR_BIN\" mcp demo pid; \"$KELIR_BIN\" mcp demo pid;          \"$KELIR_BIN\" mcp demo echo text=x; \"$KELIR_BIN\" mcp ls",
    )
    .await;
    let result = payload_text(&run.one_call("call_settled", "tool")["result"]);
    let pids: Vec<u32> = result
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect();
    assert_eq!(pids.len(), 2, "{result}");
    // `ls` never starts a server: idle before the first call, running after.
    let states: Vec<&str> = result.lines().filter(|l| l.starts_with("demo")).collect();
    assert_eq!(states.len(), 2, "{result}");
    assert!(states[0].contains("idle"), "{states:?}");
    assert!(
        states[1].contains("running") && states[1].contains("9 tools"),
        "{states:?}"
    );
    assert_eq!(pids[0], pids[1], "one server for the whole session");
    assert_eq!(
        run.events_of("call_started")
            .iter()
            .filter(|e| e["kind"] == "mcp")
            .count(),
        3
    );
    assert_eq!(
        run.events_of("call_settled")
            .iter()
            .filter(|e| e["kind"] == "mcp")
            .count(),
        3
    );
    // The session has exited: so has the server.
    wait_gone(pids[0]).await;
}

#[tokio::test]
async fn timeout_is_exit_4_and_the_call_is_not_retried_in_a_session() {
    let files = Files::new();
    let log = files.path("calls.log");
    let log_text = log.to_string_lossy().into_owned();
    let toml = server(&format!(
        "timeout = 1\nenv = {{ MCP_TEST_LOG = '{log_text}' }}"
    ));
    let run = session_run(
        &files,
        &toml,
        "\"$KELIR_BIN\" mcp demo slow seconds=10; echo rc=$?",
    )
    .await;
    let result = payload_text(&run.one_call("call_settled", "tool")["result"]);
    assert!(result.contains("rc=4"), "{result}");
    let settled = run.one_call("call_settled", "mcp");
    assert_eq!(settled["outcome"], "timeout", "{settled}");
    assert_eq!(settled["exit_code"], 4);
    let slow = files
        .lines("calls.log")
        .into_iter()
        .filter(|line| line.starts_with("slow"))
        .count();
    assert_eq!(slow, 1, "never retried");
}

#[tokio::test]
async fn a_crashed_server_is_exit_3_and_the_next_call_restarts_it() {
    let files = Files::new();
    let run = session_run(
        &files,
        &server(""),
        "\"$KELIR_BIN\" mcp demo crash; echo rc=$?; \"$KELIR_BIN\" mcp demo echo text=back; echo rc=$?",
    )
    .await;
    let result = payload_text(&run.one_call("call_settled", "tool")["result"]);
    assert!(result.contains("rc=3"), "{result}");
    assert!(
        result.contains("back") && result.contains("rc=0"),
        "{result}"
    );
    let outcomes: Vec<String> = run
        .events_of("call_settled")
        .iter()
        .filter(|e| e["kind"] == "mcp")
        .map(|e| e["outcome"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(outcomes, ["unavailable", "ok"], "{result}");
}

#[tokio::test]
async fn an_mcp_result_is_snapshotted_in_the_provider_request() {
    let files = Files::new();
    let toml = server("");
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_m",
            "bash",
            &json!({"command": "\"$KELIR_BIN\" mcp demo echo text=snapshot-me"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let config = files.mcp_toml(&toml);
    let run = run_task(
        &mock,
        &["run", "--json", "use the server"],
        &[("KELIR_CONFIG", config.as_str())],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_requests_snapshot("mcp_result", &mock.requests());
}

#[tokio::test]
async fn a_tool_error_in_a_session_is_exit_1_and_logged_as_error() {
    let files = Files::new();
    let run = session_run(
        &files,
        &server(""),
        "\"$KELIR_BIN\" mcp demo fail; echo rc=$?",
    )
    .await;
    let result = payload_text(&run.one_call("call_settled", "tool")["result"]);
    assert!(
        result.contains("the tool says no") && result.contains("rc=1"),
        "{result}"
    );
    assert_eq!(run.one_call("call_settled", "mcp")["outcome"], "error");
}

#[tokio::test]
async fn a_channel_server_is_connected_at_start_and_its_message_is_a_source_event() {
    let files = Files::new();
    let toml = server("channel = true\nenv = { MCP_TEST_CHANNEL = 'hello from the channel' }");
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("idle", 0)))]).await;
    let mut live =
        LiveSession::start_with_mcp(&mock, &["run", "--keep", "--json", "go"], "", &toml).await;
    let _ = files;
    let source = live.wait_for("source").await;
    assert_eq!(source["kind"], "channel", "{source}");
    assert_eq!(source["name"], "demo");
    let text = payload_text(&source["text"]);
    assert!(text.contains("hello from the channel"), "{text}");
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

#[tokio::test]
async fn a_socket_client_cannot_forge_a_settlement() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("idle", 0)))]).await;
    let live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    let address = kelir::ipc::value(&live.session_dir, &live.id);
    let forged = kelir::ipc::Request::new(
        "mcp-settle",
        json!({"mcp_id": "mcp_x", "key": "guess", "output": {}}),
        "mcp-internal",
    )
    // With the real token, so it is the key that stops it, not the socket.
    .with_token(kelir::trust::read_token(&live.session_dir));
    let answer = kelir::ipc::request_value(&address, &forged).await.unwrap();
    assert!(!answer.ok, "{answer:?}");
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

/// Kill the session outright (no clean shutdown): the server must go with it.
#[tokio::test]
async fn a_server_does_not_outlive_a_session_that_is_killed() {
    let files = Files::new();
    let pidfile = files.path("server.pid");
    let pidfile_text = pidfile.to_string_lossy().into_owned();
    let toml = server(&format!("env = {{ MCP_TEST_PIDFILE = '{pidfile_text}' }}"));
    let mock = Mock::start(vec![Reply::Sse(sse(&tool_turn(
        "call_k",
        "bash",
        &json!({"command": "\"$KELIR_BIN\" mcp demo echo text=a; sleep 60"}),
    )))])
    .await;
    let mut live =
        LiveSession::start_with_mcp(&mock, &["run", "--keep", "--json", "go"], "", &toml).await;
    live.wait_for("call_settled").await;
    let pid: u32 = std::fs::read_to_string(&pidfile)
        .expect("the server wrote its pid")
        .trim()
        .parse()
        .unwrap();
    assert!(pid_alive(pid));
    let session = live.pid();
    #[cfg(windows)]
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &session.to_string()])
        .output();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("kill")
        .args(["-9", &session.to_string()])
        .output();
    wait_gone(pid).await;
}

/// N2: `notifications/tools/list_changed` refreshes the broker's tool list.
#[tokio::test]
async fn list_changed_refreshes_the_tools() {
    let files = Files::new();
    let toml = server("env = { MCP_TEST_LISTCHANGED = \"1\" }");
    let run = session_run(
        &files,
        &toml,
        "\"$KELIR_BIN\" mcp demo echo text=a; sleep 1; \"$KELIR_BIN\" mcp demo late",
    )
    .await;
    let settled: Vec<_> = run
        .events_of("call_settled")
        .into_iter()
        .filter(|e| e["kind"] == "mcp")
        .collect();
    assert_eq!(settled.len(), 2, "{:?}", run.event_types());
    assert!(settled.iter().all(|e| e["outcome"] == "ok"), "{settled:?}");
}

/// N3: `call` reaches a tool that the listing would shadow.
#[tokio::test]
async fn call_reaches_a_tool_named_like_a_keyword() {
    let files = Files::new();
    let toml = format!(
        "[mcp.servers.evil]\ncommand = '{}'\nargs = ['{}']\n",
        python(),
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/support/mcp_evil.py")
    );
    let out = one_shot(&files, &toml, &["evil", "call", "ls"], &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
}
