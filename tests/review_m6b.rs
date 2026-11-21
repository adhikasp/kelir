//! M6 round-2 review (m6b): adversarial cases against the broker token, the
//! "secrets are never printed" guarantee, and the "no orphan servers" rule.
//! Reviewer-owned. Server: `tests/support/m6b_server.py`.
//!
//! Tests carrying `#[ignore = "m6b-Fn"]` prove a finding of
//! `docs/reviews/m6b-review.md`; run them with `--run-ignored all`.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]
mod support;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::*;

const SERVER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/m6b_server.py");

fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

fn toml_for(args_extra: &str, env: &[(&str, &str)]) -> String {
    let env_text = env
        .iter()
        .map(|(k, v)| format!("{k} = '{v}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "[servers.s]\ncommand = '{}'\nargs = ['{}'{args_extra}]\ndescription = \"m6b server\"\nenv = {{ {env_text} }}\n",
        python(),
        SERVER
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
    fn text(&self, name: &str) -> String {
        self.path(name).to_string_lossy().into_owned()
    }
    fn mcp_toml(&self, text: &str) -> String {
        let path = self.path("mcp.toml");
        std::fs::write(&path, text).unwrap();
        path.to_string_lossy().into_owned()
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
        // A zombie is gone for our purposes (state Z).
        std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .map(|out| {
                let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
                out.status.success() && !state.is_empty() && !state.starts_with('Z')
            })
            .unwrap_or(false)
    }
}

fn kill_pid(pid: u32) {
    #[cfg(windows)]
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .output();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .output();
}

/// Poll until `pid` is gone; returns whether it went within `limit`.
async fn gone_within(pid: u32, limit: Duration) -> bool {
    let started = Instant::now();
    while pid_alive(pid) {
        if started.elapsed() > limit {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    true
}

async fn read_pid(path: &Path) -> u32 {
    let started = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && let Ok(pid) = text.trim().parse()
        {
            return pid;
        }
        assert!(started.elapsed() < CEILING, "no pid in {path:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn tree_text(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
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
                out.push((path, String::from_utf8_lossy(&bytes).into_owned()));
            }
        }
    }
    out
}

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

// ------------------------------------------------------------------- token

/// Another session's token opens nothing here; a token holder who lacks the
/// broker key cannot forge a settlement or a channel message; nothing refused is
/// logged and no server starts.
#[tokio::test]
async fn another_sessions_token_and_a_missing_key_are_refused() {
    let files = Files::new();
    let pidfile = files.text("srv.pid");
    let toml = toml_for("", &[("M6B_PIDFILE", &pidfile)]);
    let mock_a = Mock::start(vec![Reply::Sse(sse(&text_turn("idle", 0)))]).await;
    let mock_b = Mock::start(vec![Reply::Sse(sse(&text_turn("idle", 0)))]).await;
    let mut a =
        LiveSession::start_with_mcp(&mock_a, &["run", "--keep", "--json", "go"], "", &toml).await;
    let mut b =
        LiveSession::start_with_mcp(&mock_b, &["run", "--keep", "--json", "go"], "", &toml).await;
    a.wait_for("turn_ended").await;
    b.wait_for("turn_ended").await;
    let token_a = kelir::trust::read_token(&a.session_dir).unwrap();
    let token_b = kelir::trust::read_token(&b.session_dir).unwrap();
    assert_ne!(token_a, token_b);
    let call = json!({"op": "call", "server": "s", "tool": "echo", "args": {"text": "x"}});
    // B's token against A's socket.
    let answer = raw_request(
        &a,
        json!({"verb": "mcp", "from": "mcp", "token": token_b, "args": call}),
    )
    .await;
    assert_eq!(answer["ok"], false, "{answer}");
    // Token variants that must not match: empty, prefix, token + junk, other case.
    for bad in [
        String::new(),
        token_a[..token_a.len() - 1].to_string(),
        format!("{token_a}x"),
        token_a.to_uppercase(),
    ] {
        if bad == token_a {
            continue;
        }
        let answer = raw_request(
            &a,
            json!({"verb": "mcp", "from": "mcp", "token": bad, "args": call}),
        )
        .await;
        assert_eq!(answer["ok"], false, "token {bad:?} was accepted: {answer}");
    }
    // The right token but no key: the internal verbs are refused.
    for verb in ["mcp-settle", "mcp-notify"] {
        for from in ["mcp-internal", "mcp", "user"] {
            for key in [json!(null), json!(""), json!("guess")] {
                let answer = raw_request(
                    &a,
                    json!({"verb": verb, "from": from, "token": token_a,
                           "args": {"key": key, "mcp_id": "mcp_x", "server": "s",
                                    "method": "notifications/claude/channel",
                                    "params": {"content": "FORGED"},
                                    "output": {"outcome": "ok", "exit_code": 0, "bytes": 0}}}),
                )
                .await;
                assert_eq!(answer["ok"], false, "{verb} from {from}: {answer}");
            }
        }
    }
    let log = std::fs::read_to_string(a.session_dir.join("events.jsonl")).unwrap();
    assert!(
        !log.contains("mcp_started") && !log.contains("FORGED"),
        "{log}"
    );
    assert!(
        !Path::new(&pidfile).exists(),
        "a refused request started the server"
    );
    // The right token still works, so the refusals above were about the token.
    let answer = raw_request(
        &a,
        json!({"verb": "mcp", "from": "mcp", "token": token_a, "args": call}),
    )
    .await;
    assert_eq!(answer["ok"], true, "{answer}");
    drop(b);
    drop(a);
}

/// A bash child: `KELIR_TOKEN` stripped, wrong, or another value is refused with
/// exit 3 and no mcp event; the normal environment works. No silent fallback.
#[tokio::test]
async fn a_bash_child_needs_the_token_and_never_falls_back_silently() {
    let files = Files::new();
    let toml = toml_for("", &[]);
    let command = "\"$KELIR_BIN\" mcp s echo text=ok; echo ok_rc=$?; \
        env -u KELIR_TOKEN \"$KELIR_BIN\" mcp s echo text=nt 2>&1; echo notoken_rc=$?; \
        KELIR_TOKEN=wrong \"$KELIR_BIN\" mcp s echo text=wt 2>&1; echo wrongtoken_rc=$?; \
        KELIR_TOKEN= \"$KELIR_BIN\" mcp ls 2>&1; echo emptytoken_rc=$?";
    let run = session_run(&files, &toml, command).await;
    let result = match &run.one_event("tool_settled")["result"] {
        Value::String(text) => text.clone(),
        other => other["preview"].as_str().unwrap_or("").to_string(),
    };
    assert!(result.contains("ok_rc=0"), "{result}");
    assert!(result.contains("notoken_rc=3"), "{result}");
    assert!(result.contains("wrongtoken_rc=3"), "{result}");
    assert!(result.contains("emptytoken_rc=3"), "{result}");
    assert_eq!(
        run.events_of("mcp_started").len(),
        1,
        "only the authenticated call is logged"
    );
    assert!(
        !result.contains("nt\n") || result.contains("refused"),
        "{result}"
    );
}

// ----------------------------------------------------------------- secrets

/// A canary in the server's `env`, in its `args`, in tool arguments and in a
/// tool result. Which channel may hold which:
///  - env: nowhere (not in kelir output, not in any file, not in the server's argv);
///  - server args: nowhere in kelir output (the server's own argv has it, by
///    construction);
///  - tool argument: only in the bash call that carried it (`tool_started`,
///    its out file); never in `mcp_started` / `mcp_settled`;
///  - tool result: only in the bash call's output and the spilled file.
#[tokio::test]
async fn canaries_stay_in_their_own_channels() {
    let files = Files::new();
    let env_c = "CANARY-ENV-m6b-5521";
    let arg_c = "CANARY-SRVARG-m6b-7710";
    let tool_c = "CANARY-TOOLARG-m6b-3348";
    let pidfile = files.text("srv.pid");
    let toml = toml_for(
        &format!(", '--token={arg_c}'"),
        &[("M6B_SECRET", env_c), ("M6B_PIDFILE", &pidfile)],
    );
    let command = format!(
        "\"$KELIR_BIN\" mcp ls; \"$KELIR_BIN\" mcp ls --json; \"$KELIR_BIN\" mcp s ls --detail full; \
         \"$KELIR_BIN\" mcp s echo token={tool_c} text=x; \"$KELIR_BIN\" mcp s nosuchtool 2>&1; \
         \"$KELIR_BIN\" mcp s echo token=1 2>&1; \"$KELIR_BIN\" mcp s echo --schema; echo end"
    );
    let run = session_run(&files, &toml, &command).await;
    let everything = format!(
        "{}\n{}\n{}",
        run.stdout,
        run.stderr,
        serde_json::to_string(&run.events).unwrap()
    );
    assert!(
        !everything.contains(env_c),
        "env canary in stdout/stderr/events"
    );
    assert!(
        !everything.contains(arg_c),
        "server-arg canary in stdout/stderr/events"
    );
    // Per event type.
    for event in &run.events {
        let kind = event["type"].as_str().unwrap_or("");
        let text = event.to_string();
        if text.contains(tool_c) {
            assert!(
                matches!(kind, "tool_started" | "tool_settled" | "assistant"),
                "tool-arg canary in a `{kind}` event: {text}"
            );
        }
    }
    for event in run.events_of("mcp_started") {
        assert!(!event.to_string().contains(tool_c), "{event}");
    }
    // Session files: the env and server-arg canaries are in none of them.
    for (path, text) in tree_text(&run.session_dir) {
        assert!(!text.contains(env_c), "env canary in {path:?}");
        assert!(!text.contains(arg_c), "server-arg canary in {path:?}");
    }
    // The server's own command line: no env canary (the server arg canary is
    // there by construction, which is why secrets belong in `env`).
}

/// The server child's command line (read by the server itself, live) holds the
/// server's args but never its `env` values.
#[tokio::test]
async fn the_servers_command_line_has_no_env_values() {
    let files = Files::new();
    let env_c = "CANARY-ENV-m6b-9904";
    let pidfile = files.text("srv.pid");
    let toml = toml_for("", &[("M6B_SECRET", env_c), ("M6B_PIDFILE", &pidfile)]);
    let mock = Mock::start(vec![Reply::Sse(sse(&tool_turn(
        "call_c",
        "bash",
        &json!({"command": "\"$KELIR_BIN\" mcp s argv >/dev/null; sleep 60"}),
    )))])
    .await;
    let mut live =
        LiveSession::start_with_mcp(&mock, &["run", "--keep", "--json", "go"], "", &toml).await;
    live.wait_for("mcp_settled").await;
    let pid = read_pid(Path::new(&pidfile)).await;
    #[cfg(windows)]
    let cmdline = String::from_utf8_lossy(
        &std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!("(Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').CommandLine"),
            ])
            .output()
            .unwrap()
            .stdout,
    )
    .into_owned();
    #[cfg(target_os = "macos")]
    let cmdline = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    #[cfg(not(any(windows, target_os = "macos")))]
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
        .unwrap_or_default();
    assert!(cmdline.contains("m6b_server.py"), "cmdline: {cmdline}");
    assert!(
        !cmdline.contains(env_c),
        "env value on the server's command line"
    );
    kill_pid(live.pid());
}

/// Configuration errors never echo a value, for many ways to break a file
/// around a secret (all through the library parser the session and the CLI use).
#[test]
fn config_errors_never_echo_a_secret() {
    let secret = "sk-CANARY-fuzz-0042";
    let shapes: Vec<String> = vec![
        format!("[servers.a]\ncommand='x'\nenv = {{ T = \"{secret}\" , }} oops"),
        format!("[servers.a]\ncommand='x'\nenv = {{ T = \"{secret}\" \n}}"),
        format!("[servers.a]\ncommand='x'\nenv = {{ T = '{secret}\n}}"),
        format!("[servers.a]\ncommand='x'\nenv = {{ T = \"{secret}\", T = \"{secret}\" }}"),
        format!("[servers.a]\ncommand='x'\nenv = {{ T = {secret} }}"),
        format!("[servers.a]\ncommand='x'\nenv = {{ \"{secret}\" = 1 }}"),
        format!("[servers.a]\ncommand='x'\nenv = {{ {secret} = \"v\" }}"),
        format!("[servers.a]\ncommand='x'\nenv = [\"{secret}\"]"),
        format!("[servers.a]\ncommand='x'\nenv = \"{secret}\""),
        format!("[servers.a]\ncommand={{ x = \"{secret}\" }}"),
        format!("[servers.a]\ncommand='x'\nargs = \"{secret}\""),
        format!("[servers.a]\ncommand='x'\nargs = [1, \"{secret}\"]"),
        format!("[servers.a]\ncommand='x'\nchannel = \"{secret}\""),
        format!("[servers.a]\ncommand='x'\ntimeout = \"{secret}\""),
        format!(
            "[servers.a]\ncommand='x'\ntimeout = -1\ndescription = \"{secret}\"\nbogus = \"{secret}\""
        ),
        format!("[servers.{secret}]\ncommand='x'"),
        format!("[servers.a]\ncommand='x'\nenv = {{ T = \"\\q{secret}\" }}"),
        format!("[servers.a]\ncommand='x'\nenv = {{ T = \"{secret}\\u00\" }}"),
        format!("[servers.a]\ncommand='x'\nenv = {{ T = \"{secret}\\uD800\" }}"),
        format!("[servers.a]\ncommand='x'\nenv = {{ T = 0x{secret} }}"),
        format!("[servers.a]\ncommand='x'\nenv = {{ T = 1979-05-27T07:32:00Z{secret} }}"),
        format!("[servers.a]\ncommand='x'\nenv = {{ T = \"\"\"{secret}\n"),
        format!("[servers.a]\ncommand='x'\nenv.T = \"{secret}\"\nenv.T = \"{secret}\""),
        format!(
            "[servers.a]\ncommand='x'\n[servers.a.env]\nT = \"{secret}\"\n[servers.a.env]\nT = \"{secret}\""
        ),
        format!("[servers.a]\ncommand='x'\nenv = {{ T = \"{secret}\" }}\n[servers.a]\ncommand='y'"),
        format!("servers = \"{secret}\""),
        format!("servers = {{ a = \"{secret}\" }}"),
        format!("[servers]\na = \"{secret}\""),
        format!("[servers.a]\ncommand = \"{secret}\"\nenv = 7"),
        format!("[servers.a]\ncommand = 5\nenv = {{ T = \"{secret}\" }}"),
        format!("\u{feff}[servers.a]\ncommand='x'\nenv = {{ T = \"{secret}\"}} #"),
        format!("[servers.a]\ncommand='x'\nenv = {{ T = \"{secret}\" }}\n{secret}"),
        format!("[servers.a]\ncommand='x'\n{secret} = 1"),
        format!("[servers.a]\ncommand='x'\nenv = {{ T = \"{secret}, expected a string\" , }}"),
    ];
    for (n, text) in shapes.iter().enumerate() {
        if let Err(error) = kelir::mcp::config::parse_layer(text, "mcp.toml") {
            // A key NAME may legitimately be reported (`unknown field`), but a
            // value never; for shapes where the secret is a key, the message
            // may name it, so only value-position shapes are asserted below.
            let value_position = !matches!(n, 5 | 6 | 15 | 32);
            if value_position {
                assert!(
                    !error.contains(secret),
                    "shape {n} echoed the secret: {error}\n--- source ---\n{text}"
                );
            }
        }
    }
}

/// A server that prints its configuration when it fails to start: its stderr
/// tail goes into the error shown to the model and into `kelir mcp ls`.
#[tokio::test]
async fn a_server_that_echoes_its_env_on_death_is_redacted() {
    let files = Files::new();
    let env_c = "CANARY-ENV-m6b-DEAD-1212";
    let toml = toml_for("", &[("M6B_SECRET", env_c), ("M6B_DIE_ECHO", "1")]);
    let command = "\"$KELIR_BIN\" mcp s echo text=x 2>&1; \"$KELIR_BIN\" mcp ls 2>&1; \
                   \"$KELIR_BIN\" mcp s ls 2>&1; echo end";
    let run = session_run(&files, &toml, command).await;
    let everything = format!(
        "{}{}{}",
        run.stdout,
        run.stderr,
        serde_json::to_string(&run.events).unwrap()
    );
    assert!(
        !everything.contains(env_c),
        "the env value reached the model/log through the server's stderr tail"
    );
}

// ----------------------------------------------------------------- orphans

/// The session is killed outright while a call is running (the server is busy
/// and does not read stdin) and the server does not exit on stdin EOF (as the
/// TypeScript SDK's stdio transport): neither it nor its grandchild may remain.
#[tokio::test]
async fn a_killed_session_leaves_no_server_and_no_grandchild() {
    let files = Files::new();
    let pidfile = files.text("srv.pid");
    let kidfile = files.text("kid.pid");
    let toml = toml_for(
        "",
        &[
            ("M6B_PIDFILE", &pidfile),
            ("M6B_KIDFILE", &kidfile),
            ("M6B_IGNORE_EOF", "1"),
        ],
    );
    let mock = Mock::start(vec![Reply::Sse(sse(&tool_turn(
        "call_h",
        "bash",
        &json!({"command": "\"$KELIR_BIN\" mcp s spawnkid; \"$KELIR_BIN\" mcp s hold seconds=120"}),
    )))])
    .await;
    let mut live =
        LiveSession::start_with_mcp(&mock, &["run", "--keep", "--json", "go"], "", &toml).await;
    live.wait_for("mcp_settled").await; // spawnkid
    live.wait_for("mcp_started").await; // hold is running
    let server = read_pid(Path::new(&pidfile)).await;
    let kid = read_pid(Path::new(&kidfile)).await;
    assert!(pid_alive(server) && pid_alive(kid));
    kill_pid(live.pid());
    let server_gone = gone_within(server, CEILING).await;
    let kid_gone = gone_within(kid, CEILING).await;
    // Clean up whatever is left, by exact pid, before asserting.
    kill_pid(server);
    kill_pid(kid);
    assert!(
        server_gone,
        "the MCP server {server} outlived a killed session"
    );
    assert!(
        kid_gone,
        "the server's child {kid} outlived a killed session"
    );
}

/// The graceful end: when the session finishes normally, the server and its
/// grandchild are gone.
#[tokio::test]
async fn a_finished_session_leaves_no_server_and_no_grandchild() {
    let files = Files::new();
    let pidfile = files.text("srv.pid");
    let kidfile = files.text("kid.pid");
    let toml = toml_for(
        "",
        &[
            ("M6B_PIDFILE", &pidfile),
            ("M6B_KIDFILE", &kidfile),
            ("M6B_IGNORE_EOF", "1"),
        ],
    );
    let run = session_run(&files, &toml, "\"$KELIR_BIN\" mcp s spawnkid >/dev/null").await;
    drop(run);
    let server = read_pid(Path::new(&pidfile)).await;
    let kid = read_pid(Path::new(&kidfile)).await;
    let (a, b) = (
        gone_within(server, CEILING).await,
        gone_within(kid, CEILING).await,
    );
    kill_pid(server);
    kill_pid(kid);
    assert!(a, "server {server} outlived a finished session");
    assert!(b, "grandchild {kid} outlived a finished session");
}

/// A channel server's notification becomes a `source` event of kind `channel`;
/// a secret in the server's env is not in it.
#[tokio::test]
async fn a_channel_message_is_a_source_event() {
    let toml = toml_for(
        "",
        &[
            ("M6B_CHANNEL", "hello from the channel"),
            ("M6B_SECRET", "CANARY-CH-1"),
        ],
    )
    .replace("description =", "channel = true\ndescription =");
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("idle", 0)))]).await;
    let mut live =
        LiveSession::start_with_mcp(&mock, &["run", "--keep", "--json", "go"], "", &toml).await;
    let source = live.wait_for("source").await;
    assert_eq!(source["kind"], "channel", "{source}");
    assert!(source.to_string().contains("hello from the channel"));
    assert!(!source.to_string().contains("CANARY-CH-1"));
    drop(live);
}
