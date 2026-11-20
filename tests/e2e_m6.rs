//! M6 end-to-end: `kelir mcp` against a real stdio MCP server.
//!
//! The server is `tests/support/mcp_server.py` (echo, fail, slow, big, image,
//! structured, pid, crash). One-shot cases run the real binary outside a
//! session; session cases drive a mock-provider session whose bash call runs
//! `kelir mcp …` through the broker.

mod support;

use std::path::{Path, PathBuf};
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
        std::process::Command::new(Path::new(&root).join("System32").join("tasklist.exe"))
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
            started.elapsed() < Duration::from_secs(20),
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
    let toml = server("timeout = 1");
    let log = files.path("calls.log");
    let log = log.to_string_lossy().into_owned();
    let env = [("MCP_TEST_LOG", log.as_str())];

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
        &server(""),
        &["demo", "pid"],
        &[("MCP_TEST_PIDFILE", pidfile.as_str())],
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
    assert_eq!(started["args"], json!({"text": "from-the-model"}));
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
        states[1].contains("running") && states[1].contains("8 tools"),
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
    );
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
