//! MCP server isolation and token-relay tests.
#![allow(clippy::pedantic, clippy::nursery)]

use std::time::Duration;

use crate::support::*;
use serde_json::{Value, json};

use super::mcp_core::*;

/// F1 (and the guarantee behind it): a canary in the `env` block never shows up
/// in stdout, stderr, the event log or any file of the session dir, whether the
/// file is broken, the server will not start, or a call fails.
#[tokio::test]
async fn a_canary_in_env_reaches_no_output_channel() {
    let files = Files::new();
    let canary = "sk-CANARY-m6fix-7f3a";
    let command = "for c in 'mcp ls' 'mcp ls --json' 'mcp demo echo text=x' 'mcp demo nope' \
                   'mcp demo ls --detail full'; do \"$KELIR_BIN\" $c 2>&1; done; echo end";
    let broken =
        format!("[mcp.servers.demo]\ncommand = 'x'\nenv = {{ API_TOKEN = \"{canary}\" , }} oops\n");
    let wrong_type = format!(
        "[mcp.servers.demo]\ncommand = 'x'\nenv = {{ API_TOKEN = \"{canary}\", N = 5 }}\nchannel = \"{canary}\"\n"
    );
    let dies = server(&format!(
        "env = {{ API_TOKEN = \"{canary}\", MCP_TEST_DIE = \"boom\" }}"
    ));
    let fine = server(&format!("env = {{ API_TOKEN = \"{canary}\" }}"));
    for (label, toml) in [
        ("broken", broken),
        ("wrong_type", wrong_type),
        ("dies", dies),
        ("fine", fine),
    ] {
        let run = if matches!(label, "broken" | "wrong_type") {
            session_run_raw(&files, &toml, command).await
        } else {
            session_run(&files, &toml, command).await
        };
        let everything = format!(
            "{}{}{}{}",
            run.stdout,
            run.stderr,
            serde_json::to_string(&run.events).unwrap(),
            tree_text(&run.session_dir)
        );
        assert!(!everything.contains(canary), "{label}: the canary leaked");
    }
    // The same through the one-shot path.
    for toml in [
        format!("[mcp.servers.demo]\ncommand = 'x'\nenv = {{ A = \"{canary}\" }} oops\n"),
        server(&format!(
            "env = {{ API_TOKEN = \"{canary}\", MCP_TEST_DIE = \"boom\" }}"
        )),
    ] {
        for args in [&["ls"][..], &["demo", "echo", "text=x"][..]] {
            let out = one_shot(&files, &toml, args, &[]).await;
            assert!(!format!("{}{}", out.stdout, out.stderr).contains(canary));
        }
    }
}

/// F3: a server that dies while starting says why (its stderr), through the
/// handshake path and when the command line itself is wrong.
#[tokio::test]
async fn a_server_that_dies_at_start_says_why() {
    let files = Files::new();
    let toml = server("env = { MCP_TEST_DIE = \"key\" }");
    let out = one_shot(&files, &toml, &["demo", "echo", "text=x"], &[]).await;
    assert_eq!(out.code, 3, "{}", out.stderr);
    assert!(
        out.stderr.contains("boom: missing API key"),
        "{}",
        out.stderr
    );
    let bad_path = format!(
        "[mcp.servers.demo]\ncommand = '{}'\nargs = ['{}']\n",
        python(),
        files.path("nonexistent").join("server.py").display()
    );
    let out = one_shot(&files, &bad_path, &["demo", "ls"], &[]).await;
    assert_eq!(out.code, 3, "{}", out.stderr);
    assert!(out.stderr.contains("server stderr"), "{}", out.stderr);
}

/// F6a: a server gets an allow-list of the environment, plus its own `env`.
#[tokio::test]
async fn a_server_does_not_inherit_the_sessions_environment() {
    let files = Files::new();
    let toml = server("env = { OWN_TOKEN_VAR = \"1\" }");
    let out = one_shot(
        &files,
        &toml,
        &["demo", "envnames"],
        &[
            ("KELIR_OPENROUTER_API_KEY", "sk-not-for-servers"),
            ("OPENROUTER_API_KEY", "sk-not-for-servers"),
            ("HC_TERMINAL_ID", "t9"),
        ],
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let names: Vec<&str> = out.stdout.lines().collect();
    assert!(names.contains(&"OWN_TOKEN_VAR"), "{names:?}");
    assert!(
        names.iter().any(|name| name.eq_ignore_ascii_case("PATH")),
        "{names:?}"
    );
    for name in &names {
        let upper = name.to_ascii_uppercase();
        assert!(
            !upper.starts_with("KELIR_") && !upper.starts_with("HC_") && !upper.contains("API_KEY"),
            "{name} leaked into the server"
        );
    }
}

/// F5: once a call request has reached the session, a transport failure is
/// exit 4 and the call is never run one-shot (the tool may have run).
#[tokio::test]
async fn a_lost_session_after_the_send_is_exit_4_and_not_run_again() {
    let files = Files::new();
    let log = files.path("calls.log");
    let toml = server(&format!(
        "env = {{ MCP_TEST_LOG = '{}' }}",
        log.to_string_lossy()
    ));
    let dir = files.path("fake-session");
    std::fs::create_dir_all(&dir).unwrap();
    let id = "ses_fake_m6fix";
    let listener = kelir::ipc::bind(&dir, id).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<kelir::ipc::Incoming>();
    let trust = std::sync::Arc::new(kelir::trust::Trust::new(dir.clone(), "fake-token".into()));
    kelir::ipc::serve(listener, trust, tx);
    tokio::spawn(async move {
        while let Some(incoming) = rx.recv().await {
            match incoming.request.args["op"].as_str() {
                Some("tools") => {
                    let tool = json!({"name": "echo", "inputSchema": {"type": "object"}});
                    let _ = incoming.reply.send(kelir::ipc::Response::ok(
                        "mcp",
                        0,
                        "now",
                        json!({"tools": [tool]}),
                    ));
                }
                // The call: the connection drops without an answer.
                _ => drop(incoming.reply),
            }
        }
    });
    let address = kelir::ipc::value(&dir, id);
    let out = one_shot(
        &files,
        &toml,
        &["demo", "echo", "text=a"],
        &[
            ("KELIR_SOCK", address.as_str()),
            ("KELIR_TOKEN", "fake-token"),
        ],
    )
    .await;
    assert_eq!(out.code, 4, "stdout {} stderr {}", out.stdout, out.stderr);
    assert!(out.stderr.contains("not retried"), "{}", out.stderr);
    assert!(
        files.lines("calls.log").is_empty(),
        "the tool ran one-shot: {:?}",
        files.lines("calls.log")
    );
}

/// F8: a session killed while a server was running a tool leaves
/// `call_started/mcp` without `call_settled/mcp`; the resume settles it as unknown.
#[tokio::test]
async fn resume_settles_an_orphaned_mcp_call_as_unknown() {
    let toml = server("");
    let mock = Mock::start(vec![Reply::Sse(sse(&tool_turn(
        "call_o",
        "bash",
        &json!({"command": "\"$KELIR_BIN\" mcp demo slow seconds=60"}),
    )))])
    .await;
    let mut live =
        LiveSession::start_with_mcp(&mock, &["run", "--keep", "--json", "go"], "", &toml).await;
    let started = live.wait_for_call("call_started", "mcp").await;
    let mcp_id = started["id"].as_str().unwrap().to_string();
    let session = live.pid();
    #[cfg(windows)]
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &session.to_string()])
        .output();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("kill")
        .args(["-9", &session.to_string()])
        .output();
    let state = live
        .session_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let id = live.id.clone();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mock2 = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut resumed = LiveSession::start_reusing(
        state,
        &mock2,
        &["run", "--json", "--resume", id.as_str()],
        "",
    )
    .await;
    let settled = resumed.wait_for_call("call_settled", "mcp").await;
    assert_eq!(settled["id"], json!(mcp_id));
    assert_eq!(settled["outcome"], "unknown", "{settled}");
    assert_eq!(settled["exit_code"], 4);
    let log = live.log();
    let settlements = log
        .iter()
        .filter(|e| e["type"] == "call_settled" && e["kind"] == "mcp" && e["id"] == json!(mcp_id))
        .count();
    assert_eq!(settlements, 1, "exactly one settlement");
    drop(resumed);
    drop(live);
}

/// The broker is a socket verb like any other: without the session token a
/// request is refused before it can start a server or write into the log; with
/// it the call works. A canary in the `env` block and in the tool arguments
/// reaches no event, no stdout, no stderr, no answer and no session file.
#[tokio::test]
async fn broker_requests_need_the_session_token_and_no_canary_leaks() {
    let env_canary = "sk-CANARY-m6m-env-91b";
    let arg_canary = "sk-CANARY-m6m-arg-47c";
    let toml = server(&format!("env = {{ API_TOKEN = \"{env_canary}\" }}"));
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("idle", 0)))]).await;
    let mut live =
        LiveSession::start_with_mcp(&mock, &["run", "--keep", "--json", "go"], "", &toml).await;
    live.wait_for("turn_ended").await;
    let token = kelir::trust::read_token(&live.session_dir).unwrap();
    let call = json!({"op": "call", "server": "demo", "tool": "pid",
                      "args": {"secret": arg_canary, "nested": [arg_canary]}});
    let mut answers = Vec::new();
    // No token, and a wrong one: refused, nothing logged, no server started.
    for request in [
        json!({"verb": "mcp", "from": "mcp", "args": call}),
        json!({"verb": "mcp", "from": "mcp", "token": "not-the-token", "args": call}),
        json!({"verb": "mcp", "from": "mcp", "args": {"op": "ls"}}),
    ] {
        let answer = raw_request(&live, request).await;
        assert_eq!(answer["ok"], false, "{answer}");
        assert!(
            answer["error"].as_str().unwrap_or("").contains("token"),
            "{answer}"
        );
        answers.push(answer.to_string());
    }
    let before = std::fs::read_to_string(live.session_dir.join("events.jsonl")).unwrap();
    assert!(
        !before.contains("\"kind\":\"mcp\""),
        "a refused request was logged"
    );
    // With the token: the call runs.
    let answer = raw_request(
        &live,
        json!({"verb": "mcp", "from": "mcp", "token": token, "args": call}),
    )
    .await;
    assert_eq!(answer["ok"], true, "{answer}");
    answers.push(answer.to_string());
    let log = std::fs::read_to_string(live.session_dir.join("events.jsonl")).unwrap();
    assert!(log.contains("\"kind\":\"mcp\""), "{log}");
    let files = tree_text(&live.session_dir);
    let stderr = live.stderr();
    let (_, stdout) = live.end().await;
    for (label, text) in [
        ("events.jsonl", log),
        ("session files", files),
        ("stdout", stdout),
        ("stderr", stderr),
        ("answers", answers.join("\n")),
    ] {
        assert!(!text.contains(env_canary), "{label}: the env canary leaked");
        assert!(
            !text.contains(arg_canary),
            "{label}: the argument canary leaked"
        );
    }
}

/// The CLI in a bash child reads the token the way `kelir bg` does: with
/// `KELIR_TOKEN` removed from its environment the broker refuses it (exit 3,
/// never a silent one-shot fallback that would run the call unlogged).
#[tokio::test]
async fn the_cli_without_the_token_is_refused_by_the_session() {
    let files = Files::new();
    let toml = server("");
    let run = session_run(
        &files,
        &toml,
        "env -u KELIR_TOKEN \"$KELIR_BIN\" mcp demo pid; echo \"code=$?\"",
    )
    .await;
    let result = payload_text(&run.one_call("call_settled", "tool")["result"]);
    assert!(
        result.contains("refused") && result.contains("token"),
        "{result}"
    );
    assert!(
        run.events_of("call_started")
            .iter()
            .all(|e| e["kind"] != "mcp")
    );
}

pub(super) fn tree_text(dir: &std::path::Path) -> String {
    let mut out = String::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
    }
    out
}

pub(super) async fn raw_request(live: &LiveSession, request: Value) -> Value {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let stream = kelir::ipc::connect(&live.session_dir, &live.id)
        .await
        .expect("connect");
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read);
    let mut line = request.to_string();
    line.push('\n');
    write.write_all(line.as_bytes()).await.unwrap();
    write.flush().await.unwrap();
    let mut answer = String::new();
    lines.read_line(&mut answer).await.unwrap();
    serde_json::from_str(answer.trim()).unwrap_or(json!({"unparsed": answer}))
}
