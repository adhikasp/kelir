//! Test support: a scripted, in-process OpenAI-compatible SSE server and a
//! harness that drives the real `kelir` binary against it.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub enum Reply {
    /// A complete SSE body, sent with a Content-Length.
    Sse(String),
    /// The same, after a delay: lets a verb arrive while a step is in flight.
    SseAfter(Duration, String),
    /// The same, but held until the test opens the gate.
    Gated(Arc<tokio::sync::Notify>, String),
    /// An SSE head plus a partial body, then the connection closes.
    Disconnect(String),
    /// A plain HTTP error.
    Http(u16, String),
}

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    /// Names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn messages(&self) -> &[Value] {
        self.body["messages"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn roles(&self) -> Vec<&str> {
        self.messages()
            .iter()
            .map(|message| message["role"].as_str().unwrap_or(""))
            .collect()
    }
}

struct State {
    script: VecDeque<Reply>,
    requests: Vec<Recorded>,
}

pub struct Mock {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
}

impl Mock {
    pub async fn start(script: Vec<Reply>) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State {
            script: script.into(),
            requests: Vec::new(),
        }));
        let shared = state.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let state = shared.clone();
                tokio::spawn(async move {
                    handle(stream, state).await;
                });
            }
        });
        Mock { addr, state }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.state.lock().unwrap().requests.clone()
    }

    pub fn request_count(&self) -> usize {
        self.state.lock().unwrap().requests.len()
    }
}

async fn handle(mut stream: TcpStream, state: Arc<Mutex<State>>) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        match stream.read(&mut chunk).await {
            Ok(0) => return,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(_) => return,
        }
        if let Some(position) = find(&buffer, b"\r\n\r\n") {
            break position + 4;
        }
        if buffer.len() > 1_000_000 {
            return;
        }
    };

    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or("").split(' ');
    let method = request_line.next().unwrap_or("").to_string();
    let path = request_line.next().unwrap_or("").to_string();

    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim().to_string();
            if name == "content-length" {
                content_length = value.parse().unwrap_or(0);
            }
            headers.push((name, value));
        }
    }

    let mut body = buffer[header_end..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(_) => return,
        }
    }
    let body =
        serde_json::from_slice(&body[..body.len().min(content_length)]).unwrap_or(Value::Null);

    let reply = {
        let mut state = state.lock().unwrap();
        state.requests.push(Recorded {
            method,
            path,
            headers,
            body,
        });
        state.script.pop_front()
    };

    match reply {
        Some(Reply::Sse(body)) => {
            write_sse(&mut stream, &body).await;
        }
        Some(Reply::SseAfter(delay, body)) => {
            tokio::time::sleep(delay).await;
            write_sse(&mut stream, &body).await;
        }
        Some(Reply::Gated(gate, body)) => {
            gate.notified().await;
            write_sse(&mut stream, &body).await;
        }
        Some(Reply::Disconnect(partial)) => {
            let head =
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(partial.as_bytes()).await;
            let _ = stream.flush().await;
            let _ = stream.shutdown().await;
        }
        Some(Reply::Http(status, body)) => {
            let head = format!(
                "HTTP/1.1 {status} Error\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(body.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
        None => {
            let body = "no scripted reply left";
            let head = format!(
                "HTTP/1.1 500 Error\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(body.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

async fn write_sse(stream: &mut TcpStream, body: &str) {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}

// ---------------------------------------------------------------- SSE script

/// Wrap chunks as SSE and terminate with `[DONE]`.
pub fn sse(chunks: &[Value]) -> String {
    let mut out = String::new();
    for chunk in chunks {
        out.push_str("data: ");
        out.push_str(&chunk.to_string());
        out.push_str("\n\n");
    }
    out.push_str("data: [DONE]\n\n");
    out
}

pub fn delta(text: &str) -> Value {
    json!({"choices": [{"delta": {"content": text}, "finish_reason": null}]})
}

pub fn finish(reason: &str) -> Value {
    json!({"choices": [{"delta": {}, "finish_reason": reason}]})
}

pub fn usage_chunk(prompt: u64, completion: u64, cached: u64, written: u64) -> Value {
    json!({
        "choices": [],
        "usage": {
            "prompt_tokens": prompt,
            "completion_tokens": completion,
            "prompt_tokens_details": {
                "cached_tokens": cached,
                "cache_write_tokens": written
            },
            "cost": 0.00042
        }
    })
}

/// Tool-call deltas exactly as a provider streams them: id and name first,
/// arguments as text afterwards.
pub fn tool_call_chunks(id: &str, name: &str, args: &Value) -> Vec<Value> {
    vec![
        json!({"choices": [{"delta": {"tool_calls": [{
            "index": 0,
            "id": id,
            "type": "function",
            "function": {"name": name, "arguments": ""}
        }]}, "finish_reason": null}]}),
        json!({"choices": [{"delta": {"tool_calls": [{
            "index": 0,
            "function": {"arguments": args.to_string()}
        }]}, "finish_reason": null}]}),
    ]
}

/// A whole assistant turn that calls one tool and stops.
pub fn tool_turn(id: &str, name: &str, args: &Value) -> Vec<Value> {
    let mut chunks = tool_call_chunks(id, name, args);
    chunks.push(finish("tool_calls"));
    chunks.push(usage_chunk(100, 12, 0, 100));
    chunks
}

/// A whole assistant turn that calls two tools in parallel.
pub fn two_tool_turn(first: (&str, &str, Value), second: (&str, &str, Value)) -> Vec<Value> {
    let mut chunks = Vec::new();
    for (index, (id, name, args)) in [(0, first), (1, second)] {
        chunks.push(json!({"choices": [{"delta": {"tool_calls": [{
            "index": index,
            "id": id,
            "type": "function",
            "function": {"name": name, "arguments": args.to_string()}
        }]}, "finish_reason": null}]}));
    }
    chunks.push(finish("tool_calls"));
    chunks.push(usage_chunk(120, 20, 0, 120));
    chunks
}

pub fn text_turn(text: &str, cached: u64) -> Vec<Value> {
    vec![delta(text), finish("stop"), usage_chunk(200, 8, cached, 40)]
}

// ------------------------------------------------------------------- harness

pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub events: Vec<Value>,
    pub session_dir: PathBuf,
    pub cwd: PathBuf,
    _temp: tempfile::TempDir,
}

impl Run {
    /// The NDJSON lines the binary wrote to stdout.
    pub fn json_lines(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|_| panic!("stdout line is not JSON: {line}"))
            })
            .collect()
    }

    pub fn event_types(&self) -> Vec<String> {
        self.events
            .iter()
            .map(|event| event["type"].as_str().unwrap_or("").to_string())
            .collect()
    }

    pub fn events_of(&self, event_type: &str) -> Vec<&Value> {
        self.events
            .iter()
            .filter(|event| event["type"] == event_type)
            .collect()
    }

    pub fn one_event(&self, event_type: &str) -> &Value {
        let found = self.events_of(event_type);
        assert_eq!(
            found.len(),
            1,
            "expected exactly one {event_type} event, got {}; stderr: {}",
            found.len(),
            self.stderr
        );
        found[0]
    }

    pub fn result(&self) -> Value {
        self.json_lines()
            .into_iter()
            .find(|line| line["type"] == "result")
            .expect("a result line on stdout")
    }

    pub fn command_out(&self, call_id: &str) -> String {
        let path = self.session_dir.join("commands").join(call_id).join("out");
        std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
    }
}

pub async fn run_task(
    mock: &Mock,
    args: &[&str],
    env: &[(&str, &str)],
    stdin: Option<&str>,
    extra_config: &str,
) -> Run {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        format!("provider = \"openrouter\"\nmodel = \"test/model\"\n{extra_config}"),
    )
    .unwrap();

    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_kelir"));
    command
        .args(args)
        .current_dir(&cwd)
        .env("KELIR_CONFIG", &config)
        .env("KELIR_CONFIG_DIR", temp.path().join("no-config"))
        .env("KELIR_STATE_DIR", temp.path().join("state"))
        .env("KELIR_BASE_URL", mock.base_url())
        .env("OPENROUTER_API_KEY", "test-openrouter-key")
        .env("KELIR_GO_API_KEY", "test-go-key")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }

    let mut child = command.spawn().expect("spawn kelir");
    if let Some(input) = stdin {
        let mut handle = child.stdin.take().expect("stdin");
        handle.write_all(input.as_bytes()).await.unwrap();
        drop(handle);
    }
    let output = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
        .await
        .expect("kelir did not finish within 60s")
        .expect("wait for kelir");

    let sessions = temp.path().join("state").join("sessions");
    let session_dir = std::fs::read_dir(&sessions)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| path.is_dir())
        .unwrap_or_else(|| sessions.clone());
    let events_text = std::fs::read_to_string(session_dir.join("events.jsonl")).unwrap_or_default();
    let events = events_text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("bad event line ({error}): {line}"))
        })
        .collect();

    Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        events,
        session_dir,
        cwd,
        _temp: temp,
    }
}

// --------------------------------------------------- a running session + ctl

/// A `kelir run` (usually `--keep`) that stays up while the test drives it, and
/// the `kelir ctl` surface that talks to it.
pub struct LiveSession {
    child: tokio::process::Child,
    stdout: tokio::io::BufReader<tokio::process::ChildStdout>,
    stderr: Arc<Mutex<String>>,
    stderr_task: tokio::task::JoinHandle<()>,
    _temp: tempfile::TempDir,
    pub cwd: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    pub id: String,
    pub session_dir: PathBuf,
    /// Every NDJSON line read from the session so far.
    pub events: Vec<Value>,
}

pub struct Ctl {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Ctl {
    /// The one JSON response line the session wrote back.
    pub fn response(&self) -> Value {
        self.stdout
            .lines()
            .rfind(|line| line.trim_start().starts_with('{'))
            .unwrap_or_else(|| panic!("no JSON answer; stderr: {}", self.stderr))
            .parse()
            .unwrap_or_else(|error| panic!("answer is not JSON ({error}); stderr: {}", self.stderr))
    }
}

/// Is `pid` still a running process? (The support module's own answer: the e2e
/// tests carry their own copy; this one is for a diagnostic message.)
fn pid_alive_here(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let tasklist = std::path::Path::new(&system_root)
            .join("System32")
            .join("tasklist.exe");
        std::process::Command::new(tasklist)
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\"")))
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("ps")
            .args(["-p", &pid.to_string()])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
}

impl LiveSession {
    pub async fn start(mock: &Mock, args: &[&str], extra_config: &str) -> LiveSession {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        let config = temp.path().join("config.toml");
        std::fs::write(
            &config,
            format!("provider = \"openrouter\"\nmodel = \"test/model\"\n{extra_config}"),
        )
        .unwrap();
        let state = temp.path().join("state");

        let mut command = live_command(&cwd, &config, &state, &mock.base_url());
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().expect("spawn kelir");
        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");
        let buffer = Arc::new(Mutex::new(String::new()));
        let task = tokio::spawn(drain(stderr, buffer.clone()));

        let mut session = LiveSession {
            child,
            stdout: tokio::io::BufReader::new(stdout),
            stderr: buffer,
            stderr_task: task,
            _temp: temp,
            cwd,
            config,
            state,
            id: String::new(),
            session_dir: PathBuf::new(),
            events: Vec::new(),
        };
        let first = session.next_event().await;
        assert_eq!(first["type"], "session_started", "{first}");
        session.id = first["id"].as_str().expect("session id").to_string();
        session.session_dir = session.state.join("sessions").join(&session.id);
        session
    }

    /// The next NDJSON event the session wrote.
    pub async fn next_event(&mut self) -> Value {
        let pid = self.pid();
        let started = std::time::Instant::now();
        let mut line = String::new();
        let read = tokio::time::timeout(Duration::from_secs(30), async {
            use tokio::io::AsyncBufReadExt as _;
            self.stdout.read_line(&mut line).await
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "no event within 30 s (session pid {pid} alive={} after {:?}, seen so far: {:?}, stderr: {})",
                pid_alive_here(pid),
                started.elapsed(),
                self.events
                    .iter()
                    .map(|event| event["type"].clone())
                    .collect::<Vec<_>>(),
                self.stderr()
            )
        })
        .expect("read the session output");
        assert!(
            read > 0,
            "the session closed its output; stderr: {}",
            self.stderr()
        );
        let value: Value = serde_json::from_str(line.trim())
            .unwrap_or_else(|error| panic!("bad event line ({error}): {line}"));
        self.events.push(value.clone());
        value
    }

    /// Read until an event of this type arrives (never looks backwards).
    pub async fn wait_for(&mut self, event_type: &str) -> Value {
        loop {
            let event = self.next_event().await;
            assert_ne!(
                event["type"], "result",
                "the session ended before a {event_type} event"
            );
            if event["type"] == event_type {
                return event;
            }
        }
    }

    /// `kelir ctl <this session> …`.
    pub async fn ctl(&self, args: &[&str]) -> Ctl {
        let mut full = vec![self.id.as_str()];
        full.extend_from_slice(args);
        ctl_in(&self.state, &self.cwd, &self.config, &full).await
    }

    /// `kelir ctl latest …`: exercises session resolution.
    pub async fn ctl_latest(&self, args: &[&str]) -> Ctl {
        let mut full = vec!["latest"];
        full.extend_from_slice(args);
        ctl_in(&self.state, &self.cwd, &self.config, &full).await
    }

    pub fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    /// The session process's pid, for a test that has to say whether it is
    /// still alive when an event is late.
    pub fn pid(&self) -> u32 {
        self.child.id().unwrap_or(0)
    }

    /// The session log as it is on disk right now.
    pub fn log(&self) -> Vec<Value> {
        std::fs::read_to_string(self.session_dir.join("events.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
            .collect()
    }

    /// Ask the session to end and wait for it to exit.
    pub async fn end(self) -> (i32, String) {
        let answer = self.ctl(&["end"]).await;
        assert_eq!(answer.code, 0, "end was refused: {}", answer.stdout);
        self.finish().await
    }

    /// Kill the session the way a crash or a `taskkill` would: the socket dies
    /// and the state dir stays behind, so a later `ctl` can be tried against it.
    pub async fn kill(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }

    /// Wait for the process to exit on its own.
    pub async fn finish(mut self) -> (i32, String) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        match tokio::time::timeout_at(deadline, self.child.wait()).await {
            Ok(Ok(status)) => {
                let code = status.code().unwrap_or(-1);
                let buffer = self.stderr.clone();
                let _ = self.stderr_task.await;
                (code, buffer.lock().unwrap().clone())
            }
            Ok(Err(error)) => panic!("wait for kelir: {error}"),
            Err(_) => {
                let _ = self.child.kill().await;
                panic!("kelir did not exit within 30 s; stderr: {}", self.stderr());
            }
        }
    }
}

// ------------------------------------------------------- a reusable sandbox

/// One state dir, cwd and config shared by several `kelir` invocations, so a
/// test can crash a run and resume the same session.
pub struct Sandbox {
    _temp: Option<tempfile::TempDir>,
    pub cwd: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
}

#[derive(Debug)]
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn json_lines(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|_| panic!("stdout line is not JSON: {line}"))
            })
            .collect()
    }

    pub fn result(&self) -> Option<Value> {
        self.json_lines()
            .into_iter()
            .find(|line| line["type"] == "result")
    }
}

impl Sandbox {
    pub fn new(extra_config: &str) -> Sandbox {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        let config = temp.path().join("config.toml");
        std::fs::write(
            &config,
            format!("provider = \"openrouter\"\nmodel = \"test/model\"\n{extra_config}"),
        )
        .unwrap();
        let state = temp.path().join("state");
        Sandbox {
            _temp: Some(temp),
            cwd,
            config,
            state,
        }
    }

    /// The same dirs as a running [`LiveSession`], for a second process (a
    /// resume) on its log.
    pub fn from_live(live: &LiveSession) -> Sandbox {
        Sandbox {
            _temp: None,
            cwd: live.cwd.clone(),
            config: live.config.clone(),
            state: live.state.clone(),
        }
    }

    /// Run the real binary against `base_url`, capturing stdout and stderr.
    pub async fn kelir(&self, base_url: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = live_command(&self.cwd, &self.config, &self.state, base_url);
        command.args(args);
        for (key, value) in env {
            command.env(key, value);
        }
        let output = tokio::time::timeout(Duration::from_secs(60), command.output())
            .await
            .expect("kelir did not finish within 60 s")
            .expect("run kelir");
        Output {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    pub fn session_ids(&self) -> Vec<String> {
        kelir::paths::sessions_newest_first_in(&self.state.join("sessions"))
    }

    pub fn only_session(&self) -> String {
        let ids = self.session_ids();
        assert_eq!(ids.len(), 1, "expected one session, got {ids:?}");
        ids[0].clone()
    }

    pub fn session_dir(&self, id: &str) -> PathBuf {
        self.state.join("sessions").join(id)
    }

    /// The log as it is on disk, skipping a line a crash left torn (a resume
    /// reports those; the test still wants the rest).
    pub fn log(&self, id: &str) -> Vec<Value> {
        std::fs::read_to_string(self.session_dir(id).join("events.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// The lines of a file the tools wrote, 0 when it does not exist.
    pub fn lines_in(&self, name: &str) -> usize {
        std::fs::read_to_string(self.cwd.join(name))
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }
}

fn live_command(
    cwd: &PathBuf,
    config: &PathBuf,
    state: &PathBuf,
    base_url: &str,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_kelir"));
    command
        .current_dir(cwd)
        .env("KELIR_CONFIG", config)
        .env("KELIR_CONFIG_DIR", state.join("no-config"))
        .env("KELIR_STATE_DIR", state)
        .env("KELIR_BASE_URL", base_url)
        .env("OPENROUTER_API_KEY", "test-openrouter-key")
        .env("KELIR_GO_API_KEY", "test-go-key");
    command
}

/// `kelir ctl` against a state dir, with no live session in the test harness.
pub async fn ctl_in(state: &Path, cwd: &Path, config: &Path, args: &[&str]) -> Ctl {
    let mut command = live_command(
        &cwd.to_path_buf(),
        &config.to_path_buf(),
        &state.to_path_buf(),
        "http://127.0.0.1:9/v1",
    );
    command.arg("ctl");
    for arg in args {
        command.arg(arg);
    }
    let output = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .expect("ctl did not finish within 30 s")
        .expect("run ctl");
    Ctl {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

async fn drain(mut stderr: tokio::process::ChildStderr, buffer: Arc<Mutex<String>>) {
    use tokio::io::AsyncReadExt as _;
    let mut text = Vec::new();
    let _ = stderr.read_to_end(&mut text).await;
    buffer
        .lock()
        .unwrap()
        .push_str(&String::from_utf8_lossy(&text));
}
