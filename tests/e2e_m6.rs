//! M6 end-to-end: `kelir mcp` against a real stdio MCP server.
//!
//! The server is `tests/support/mcp_server.py` (echo, fail, slow, big, image,
//! structured, pid, crash). One-shot cases run the real binary outside a
//! session; session cases drive a mock-provider session whose bash call runs
//! `kelir mcp …` through the broker.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]
mod support;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::*;

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mcp_server.py");

fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

/// A `[servers.demo]` block. `extra` is more TOML lines for the same table.
fn server(extra: &str) -> String {
    format!(
        "[servers.demo]\ncommand = '{}'\nargs = ['{}']\ndescription = \"test server\"\n{extra}\n",
        python(),
        SCRIPT
    )
}

struct Files {
    dir: tempfile::TempDir,
}

impl Files {
    fn new() -> Files {
        Files {
            dir: tempfile::tempdir().unwrap(),
        }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
    fn mcp_toml(&self, text: &str) -> String {
        let path = self.path("mcp.toml");
        std::fs::write(&path, text).unwrap();
        path.to_string_lossy().into_owned()
    }
    fn lines(&self, name: &str) -> Vec<String> {
        std::fs::read_to_string(self.path(name))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        std::process::Command::new(
            std::path::Path::new(&root)
                .join("System32")
                .join("tasklist.exe"),
        )
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\"")))
        .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("ps")
            .args(["-p", &pid.to_string()])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }
}

async fn wait_gone(pid: u32) {
    let started = Instant::now();
    while pid_alive(pid) {
        assert!(
            started.elapsed() < CEILING,
            "process {pid} is still running"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// `kelir mcp <args…>` outside any session.
async fn one_shot(files: &Files, toml: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
    let sandbox = Sandbox::new("");
    let config = files.mcp_toml(toml);
    let mut full = vec![("KELIR_MCP_CONFIG", config.as_str())];
    full.extend_from_slice(env);
    let mut argv = vec!["mcp"];
    argv.extend_from_slice(args);
    sandbox.kelir("http://127.0.0.1:9/v1", &argv, &full).await
}

fn event_index(events: &[Value], event_type: &str) -> usize {
    events
        .iter()
        .position(|event| event["type"] == event_type)
        .unwrap_or_else(|| panic!("no {event_type} event in {:?}", types(events)))
}

fn types(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| event["type"].as_str().unwrap_or("").to_string())
        .collect()
}

/// The text a `tool_settled` (or any `Payload`) holds, spilled or not.
fn payload_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Object(map) => map
            .get("preview")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        other => other.to_string(),
    }
}

/// A one-run session whose single bash call is `command`.
async fn session_run(files: &Files, toml: &str, command: &str) -> Run {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_m",
            "bash",
            &json!({"command": command}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let config = files.mcp_toml(toml);
    let run = run_task(
        &mock,
        &["run", "--json", "use the server"],
        &[("KELIR_MCP_CONFIG", config.as_str())],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    run
}

// ------------------------------------------------------------------ one-shot

#[tokio::test]
async fn ls_lists_servers_and_tools() {
    let files = Files::new();
    let toml = server("");
    let out = one_shot(&files, &toml, &["ls"], &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("demo") && out.stdout.contains("test server"),
        "{}",
        out.stdout
    );

    let out = one_shot(&files, &toml, &["ls", "--json"], &[]).await;
    let list: Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert_eq!(list[0]["name"], "demo");
    assert_eq!(list[0]["description"], "test server");

    let out = one_shot(&files, &toml, &["demo", "ls"], &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("echo  Echo the text back."),
        "{}",
        out.stdout
    );
    assert!(!out.stdout.contains("Second line"), "{}", out.stdout);

    let out = one_shot(&files, &toml, &["demo", "ls", "--detail", "name"], &[]).await;
    assert!(
        out.stdout.lines().any(|line| line == "echo"),
        "{}",
        out.stdout
    );

    let out = one_shot(&files, &toml, &["demo", "ls", "--detail", "full"], &[]).await;
    let tools: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(tools[0]["inputSchema"]["required"][0], "text");
}

#[tokio::test]
async fn a_call_prints_text_json_files_and_the_envelope() {
    let files = Files::new();
    let toml = server("");
    let out = one_shot(&files, &toml, &["demo", "echo", "text=hello world"], &[]).await;
    assert_eq!(
        (out.code, out.stdout.trim()),
        (0, "hello world"),
        "{}",
        out.stderr
    );

    // `n=2` is typed as an integer from the schema; the text is repeated.
    let out = one_shot(&files, &toml, &["demo", "echo", "text=ab", "n=2"], &[]).await;
    assert_eq!(out.stdout.trim(), "abab", "{}", out.stderr);

    let out = one_shot(
        &files,
        &toml,
        &[
            "demo",
            "echo",
            "--json",
            "{\"text\":\"x\",\"mode\":\"loud\"}",
        ],
        &[],
    )
    .await;
    assert_eq!(out.stdout.trim(), "X", "{}", out.stderr);

    let out = one_shot(&files, &toml, &["demo", "structured"], &[]).await;
    let value: Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert_eq!(value["answer"], 42);

    let out = one_shot(&files, &toml, &["demo", "structured", "--envelope"], &[]).await;
    let value: Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert!(value["content"].is_array() && value["structuredContent"]["answer"] == 42);

    let out = one_shot(&files, &toml, &["demo", "echo", "--schema"], &[]).await;
    let schema: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(schema["properties"]["text"]["type"], "string");

    // The sandbox is gone when the command returns; keep the state dir (where a
    // one-shot call puts decoded files) so the test can open the file.
    let state = files.path("state");
    let state = state.to_string_lossy().into_owned();
    let out = one_shot(
        &files,
        &toml,
        &["demo", "image"],
        &[("KELIR_STATE_DIR", state.as_str())],
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let path = out.stdout.lines().last().unwrap().trim().to_string();
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    assert_eq!(&bytes[1..4], b"PNG", "{path}");
}

#[tokio::test]
async fn exit_codes_follow_the_spec() {
    let files = Files::new();
    let log = files.path("calls.log");
    let log = log.to_string_lossy().into_owned();
    let toml = server(&format!(
        "timeout = 1
env = {{ MCP_TEST_LOG = '{log}' }}"
    ));
    let env: [(&str, &str); 0] = [];

    let out = one_shot(&files, &toml, &["demo", "fail"], &env).await;
    assert_eq!(out.code, 1, "tool isError");
    assert!(
        out.stdout.is_empty() && out.stderr.contains("the tool says no"),
        "{out:?}"
    );

    let out = one_shot(&files, &toml, &["nope", "echo"], &env).await;
    assert_eq!(out.code, 2, "unknown server");
    assert!(out.stderr.contains("unknown server `nope`") && out.stderr.contains("demo"));

    let out = one_shot(&files, &toml, &["demo", "nope"], &env).await;
    assert_eq!(out.code, 2, "unknown tool");
    assert!(out.stderr.contains("no tool `nope`") && out.stderr.contains("echo"));

    for (args, needle) in [
        (vec!["demo", "echo", "text=1", "n=abc"], "expected integer"),
        (vec!["demo", "echo", "n=1"], "missing required `text`"),
        (
            vec!["demo", "echo", "text=x", "bogus=1"],
            "unknown argument `bogus`",
        ),
        (
            vec!["demo", "echo", "text=x", "mode=quiet"],
            "is not one of",
        ),
        (vec!["demo", "echo", "loose"], "key=value"),
    ] {
        let out = one_shot(&files, &toml, &args, &env).await;
        assert_eq!(out.code, 2, "{args:?}: {out:?}");
        assert!(out.stderr.contains(needle), "{args:?}: {}", out.stderr);
    }
    // Validation happens before the server is asked.
    assert!(
        files
            .lines("calls.log")
            .iter()
            .all(|l| l.starts_with("fail")),
        "{:?}",
        files.lines("calls.log")
    );

    let out = one_shot(&files, &toml, &["demo", "crash"], &env).await;
    assert_eq!(out.code, 3, "server crash: {out:?}");

    let missing = "[servers.ghost]\ncommand = 'no-such-program-kelir-m6'\n";
    let out = one_shot(&files, missing, &["ghost", "x"], &env).await;
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stderr.contains("cannot start"), "{}", out.stderr);

    let out = one_shot(&files, &toml, &["demo", "slow", "seconds=10"], &env).await;
    assert_eq!(out.code, 4, "timeout: {out:?}");
    assert!(out.stderr.contains("not retried"), "{}", out.stderr);
    // Never retried: the server saw exactly one `slow`.
    let slow = files
        .lines("calls.log")
        .into_iter()
        .filter(|line| line.starts_with("slow"))
        .count();
    assert_eq!(slow, 1);
}

#[tokio::test]
async fn the_project_layer_adds_to_the_machine_layer_and_wins_a_name_clash() {
    let files = Files::new();
    let sandbox = Sandbox::new("");
    std::fs::create_dir_all(sandbox.cwd.join(".kelir")).unwrap();
    std::fs::write(
        sandbox.cwd.join(".kelir").join("mcp.toml"),
        "[servers.demo]
command = 'no-such-program-kelir-m6'
description = \"project\"
         [servers.extra]
command = 'x'
description = \"only in the project\"
",
    )
    .unwrap();
    let config = files.mcp_toml(&format!(
        "{}[servers.other]
command = 'y'
",
        server("")
    ));
    let out = sandbox
        .kelir(
            "http://127.0.0.1:9/v1",
            &["mcp", "ls", "--json"],
            &[("KELIR_MCP_CONFIG", config.as_str())],
        )
        .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let list: Value = serde_json::from_str(out.stdout.trim()).unwrap();
    let by_name = |name: &str| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("no {name} in {list}"))
    };
    assert_eq!(by_name("demo")["description"], "project", "project wins");
    assert_eq!(by_name("extra")["description"], "only in the project");
    assert!(
        by_name("other")["description"].is_string(),
        "machine-only survives"
    );
}

#[tokio::test]
async fn a_bad_mcp_toml_is_a_usage_error_naming_the_file() {
    let files = Files::new();
    let out = one_shot(
        &files,
        "[servers.x]\ncommand = 'a'\nbogus = 1\n",
        &["x", "t"],
        &[],
    )
    .await;
    assert_eq!(out.code, 2, "{out:?}");
    assert!(
        out.stderr.contains("bogus") && out.stderr.contains("mcp.toml"),
        "{}",
        out.stderr
    );
}

#[tokio::test]
async fn the_one_shot_server_dies_with_the_command() {
    let files = Files::new();
    let pidfile = files.path("pid");
    let pidfile = pidfile.to_string_lossy().into_owned();
    let out = one_shot(
        &files,
        &server(&format!("env = {{ MCP_TEST_PIDFILE = '{pidfile}' }}")),
        &["demo", "pid"],
        &[],
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let pid: u32 = out.stdout.trim().parse().expect("the server's pid");
    wait_gone(pid).await;
}

// ------------------------------------------------------------------- session

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
        event_index(&run.events, "tool_started"),
        event_index(&run.events, "mcp_started"),
        event_index(&run.events, "mcp_settled"),
        event_index(&run.events, "tool_settled"),
    );
    assert!(a < b && b < c && c < d, "{kinds:?}");

    let started = run.one_event("mcp_started");
    assert_eq!(started["server"], "demo");
    assert_eq!(started["tool"], "echo");
    // The shape, never the values (m6m): the text is in the bash call's tool_started.
    assert_eq!(started["args"], json!({"text": "<string, 14 bytes>"}));
    assert_eq!(started["parent_call_id"], "call_m");
    let settled = run.one_event("mcp_settled");
    assert_eq!(settled["mcp_id"], started["mcp_id"]);
    assert_eq!(settled["outcome"], "ok");
    assert_eq!(settled["exit_code"], 0);
    assert_eq!(settled["parent_call_id"], "call_m");
    let out_path = settled["out_path"].as_str().unwrap();
    assert!(
        out_path.contains("commands"),
        "the result lives in the session dir: {out_path}"
    );
    assert_eq!(std::fs::read_to_string(out_path).unwrap(), "from-the-model");

    let tool = run.one_event("tool_settled");
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
    let tool = run.one_event("tool_settled");
    let counted: u64 = payload_text(&tool["result"])
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("wc -c printed {}", payload_text(&tool["result"])));
    assert!(counted >= 20_000, "{counted}");
    let settled = run.one_event("mcp_settled");
    assert!(settled["bytes"].as_u64().unwrap() >= 20_000, "{settled}");
    let full = std::fs::read_to_string(settled["out_path"].as_str().unwrap()).unwrap();
    assert!(full.len() >= 20_000);

    // Not piped: the bash call's own 10 KB rule gives the model a notice.
    let run = session_run(&files, &server(""), "\"$KELIR_BIN\" mcp demo big").await;
    let tool = run.one_event("tool_settled");
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
    let result = payload_text(&run.one_event("tool_settled")["result"]);
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
    assert_eq!(run.events_of("mcp_started").len(), 3);
    assert_eq!(run.events_of("mcp_settled").len(), 3);
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
    let result = payload_text(&run.one_event("tool_settled")["result"]);
    assert!(result.contains("rc=4"), "{result}");
    let settled = run.one_event("mcp_settled");
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
    let result = payload_text(&run.one_event("tool_settled")["result"]);
    assert!(result.contains("rc=3"), "{result}");
    assert!(
        result.contains("back") && result.contains("rc=0"),
        "{result}"
    );
    let outcomes: Vec<String> = run
        .events_of("mcp_settled")
        .iter()
        .map(|e| e["outcome"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(outcomes, ["unavailable", "ok"], "{result}");
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
    let result = payload_text(&run.one_event("tool_settled")["result"]);
    assert!(
        result.contains("the tool says no") && result.contains("rc=1"),
        "{result}"
    );
    assert_eq!(run.one_event("mcp_settled")["outcome"], "error");
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
    live.wait_for("mcp_settled").await;
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

// ------------------------------------------------------------- m6fix

/// Every file under `dir`, as lossy text, for a canary scan.
fn tree_text(dir: &std::path::Path) -> String {
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
        format!("[servers.demo]\ncommand = 'x'\nenv = {{ API_TOKEN = \"{canary}\" , }} oops\n");
    let wrong_type = format!(
        "[servers.demo]\ncommand = 'x'\nenv = {{ API_TOKEN = \"{canary}\", N = 5 }}\nchannel = \"{canary}\"\n"
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
        let run = session_run(&files, &toml, command).await;
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
        format!("[servers.demo]\ncommand = 'x'\nenv = {{ A = \"{canary}\" }} oops\n"),
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
        "[servers.demo]\ncommand = '{}'\nargs = ['{}']\n",
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
    let settled = run.events_of("mcp_settled");
    assert_eq!(settled.len(), 2, "{:?}", run.event_types());
    assert!(settled.iter().all(|e| e["outcome"] == "ok"), "{settled:?}");
}

/// N3: `call` reaches a tool that the listing would shadow.
#[tokio::test]
async fn call_reaches_a_tool_named_like_a_keyword() {
    let files = Files::new();
    let toml = format!(
        "[servers.evil]\ncommand = '{}'\nargs = ['{}']\n",
        python(),
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mcp_evil.py")
    );
    let out = one_shot(&files, &toml, &["evil", "call", "ls"], &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
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
/// `mcp_started` without `mcp_settled`; the resume settles it as unknown.
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
    let started = live.wait_for("mcp_started").await;
    let mcp_id = started["mcp_id"].as_str().unwrap().to_string();
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
    let mut resumed =
        LiveSession::start_reusing(state, &mock2, &["resume", "--json", id.as_str()], "").await;
    let settled = resumed.wait_for("mcp_settled").await;
    assert_eq!(settled["mcp_id"], json!(mcp_id));
    assert_eq!(settled["outcome"], "unknown", "{settled}");
    assert_eq!(settled["exit_code"], 4);
    let log = live.log();
    let settlements = log
        .iter()
        .filter(|e| e["type"] == "mcp_settled" && e["mcp_id"] == json!(mcp_id))
        .count();
    assert_eq!(settlements, 1, "exactly one settlement");
    drop(resumed);
    drop(live);
}

// -------------------------------------------------------------------- m6m

/// One raw line to the session socket, answered as JSON.
async fn raw_request(live: &LiveSession, request: Value) -> Value {
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
        !before.contains("mcp_started"),
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
    assert!(
        log.contains("mcp_started") && log.contains("mcp_settled"),
        "{log}"
    );
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
    let result = payload_text(&run.one_event("tool_settled")["result"]);
    assert!(
        result.contains("refused") && result.contains("token"),
        "{result}"
    );
    assert!(run.events_of("mcp_started").is_empty());
}

// ------------------------------------------------------------------- m6m2

/// `ctl status` lists the MCP servers (state word, tool count, channel flag)
/// and nothing from their config or output; the canary stays out of the answer.
#[tokio::test]
async fn status_lists_mcp_servers_without_secrets() {
    let files = Files::new();
    let canary = "sk-CANARY-m6m2-91bd";
    let toml = server(&format!("env = {{ API_TOKEN = \"{canary}\" }}"));
    let command = "\"$KELIR_BIN\" mcp demo echo text=x; id=$(basename \"$KELIR_SESSION_DIR\"); \
                   \"$KELIR_BIN\" ctl \"$id\" status --json; \"$KELIR_BIN\" ctl \"$id\" status";
    let run = session_run(&files, &toml, command).await;
    let out = format!(
        "{}{}",
        run.stdout,
        serde_json::to_string(&run.events).unwrap()
    );
    assert!(!out.contains(canary), "the canary leaked");
    assert!(out.contains("mcp\\\":[{"), "{out}");
    assert!(out.contains("mcp       demo ("), "{out}");
}
