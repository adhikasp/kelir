//! M6 end-to-end: `kelir mcp` against a real stdio MCP server.
//!
//! The server is `tests/it/support/mcp_server.py` (echo, fail, slow, big, image,
//! structured, pid, crash). One-shot cases run the real binary outside a
//! session; session cases drive a mock-provider session whose bash call runs
//! `kelir mcp …` through the broker.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::support::*;
use serde_json::{Value, json};

const SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/it/support/mcp_server.py"
);

pub(super) fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

/// A `[mcp.servers.demo]` block. `extra` is more TOML lines for the same table.
pub(super) fn server(extra: &str) -> String {
    format!(
        "[mcp.servers.demo]\ncommand = '{}'\nargs = ['{}']\ndescription = \"test server\"\n{extra}\n",
        python(),
        SCRIPT
    )
}

pub(super) struct Files {
    dir: tempfile::TempDir,
}

impl Files {
    pub(super) fn new() -> Files {
        Files {
            dir: tempfile::tempdir().unwrap(),
        }
    }
    pub(super) fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
    pub(super) fn mcp_toml(&self, text: &str) -> String {
        let path = self.path("config.toml");
        std::fs::write(&path, test_config(text)).unwrap();
        path.to_string_lossy().into_owned()
    }
    pub(super) fn lines(&self, name: &str) -> Vec<String> {
        std::fs::read_to_string(self.path(name))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

pub(super) fn pid_alive(pid: u32) -> bool {
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

pub(super) async fn wait_gone(pid: u32) {
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
pub(super) async fn one_shot(
    files: &Files,
    toml: &str,
    args: &[&str],
    env: &[(&str, &str)],
) -> Output {
    let sandbox = Sandbox::new("");
    let config = files.mcp_toml(toml);
    let mut full = vec![("KELIR_CONFIG", config.as_str())];
    full.extend_from_slice(env);
    let mut argv = vec!["mcp"];
    argv.extend_from_slice(args);
    sandbox.kelir("http://127.0.0.1:9/v1", &argv, &full).await
}

pub(super) fn call_index(events: &[Value], event_type: &str, kind: &str) -> usize {
    events
        .iter()
        .position(|event| event["type"] == event_type && event["kind"] == kind)
        .unwrap_or_else(|| panic!("no {event_type}/{kind} event in {:?}", types(events)))
}

pub(super) fn types(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| event["type"].as_str().unwrap_or("").to_string())
        .collect()
}

pub(super) fn payload_text(value: &Value) -> String {
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
pub(super) async fn session_run(files: &Files, toml: &str, command: &str) -> Run {
    let run = session_run_raw(files, toml, command).await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    run
}

pub(super) async fn session_run_raw(files: &Files, toml: &str, command: &str) -> Run {
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
    run_task(
        &mock,
        &["run", "--json", "use the server"],
        &[("KELIR_CONFIG", config.as_str())],
        None,
        "",
    )
    .await
}

// ------------------------------------------------------------------ one-shot

#[tokio::test]
async fn an_explicit_config_flag_selects_the_servers_for_the_session() {
    let files = Files::new();
    // The env-named config has a decoy server; only `--config` has `demo`.
    let env_config = files.mcp_toml("[mcp.servers.decoy]\ncommand = 'decoy-program'\n");
    let explicit = files.path("alt.toml");
    std::fs::write(&explicit, test_config(&server(""))).unwrap();
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_m",
            "bash",
            &json!({"command": "\"$KELIR_BIN\" mcp ls --json"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let run = run_task(
        &mock,
        &[
            "run",
            "--json",
            "--config",
            explicit.to_str().unwrap(),
            "list servers",
        ],
        &[("KELIR_CONFIG", env_config.as_str())],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let result = payload_text(&run.one_call("call_settled", "tool")["result"]);
    assert!(
        result.contains("demo"),
        "the explicit file's server is missing: {result}"
    );
    assert!(
        !result.contains("decoy"),
        "the env-named config leaked into the broker: {result}"
    );
}

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

    let missing = "[mcp.servers.ghost]\ncommand = 'no-such-program-kelir-m6'\n";
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
        sandbox.cwd.join(".kelir").join("config.toml"),
        "[mcp.servers.demo]
command = 'no-such-program-kelir-m6'
description = \"project\"
         [mcp.servers.extra]
command = 'x'
description = \"only in the project\"
",
    )
    .unwrap();
    let config = files.mcp_toml(&format!(
        "{}[mcp.servers.other]
command = 'y'
",
        server("")
    ));
    let out = sandbox
        .kelir(
            "http://127.0.0.1:9/v1",
            &["mcp", "ls", "--json"],
            &[("KELIR_CONFIG", config.as_str())],
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
        "[mcp.servers.x]\ncommand = 'a'\nbogus = 1\n",
        &["x", "t"],
        &[],
    )
    .await;
    assert_eq!(out.code, 2, "{out:?}");
    assert!(
        out.stderr.contains("bogus") && out.stderr.contains("config.toml"),
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
