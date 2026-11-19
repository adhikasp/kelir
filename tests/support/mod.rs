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
    /// An HTTP error with headers of the test's own: `Retry-After`, a reset
    /// header, anything a provider sends to say when to come back. A `None`
    /// value sends a connection-level header without a value, which is not
    /// needed; every entry is `name: value`.
    HttpWith(u16, Vec<(String, String)>, String),
    /// `HttpWith`, held until the test opens the gate: the retry wait has not
    /// started yet, so it lets a test deliver a signal *before* the session
    /// enters the wait (STEER-5 fixes round 2).
    GatedHttpWith(u16, Vec<(String, String)>, String, Arc<tokio::sync::Notify>),
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
    /// What `GET /models` answers, and how often it was asked.
    models: ModelsAnswer,
    models_requests: u32,
    /// Completion requests in flight right now, and the highest that ever was.
    live: u32,
    peak: u32,
}

/// One completion request's lifetime, as the mock sees it: in flight from the
/// moment the request line was read until its whole answer has been written.
/// The peak of [`State::live`] is what "never overlap" is about.
struct InFlight(Arc<Mutex<State>>);

impl InFlight {
    fn new(state: Arc<Mutex<State>>) -> Self {
        {
            let mut state = state.lock().unwrap();
            state.live += 1;
            state.peak = state.peak.max(state.live);
        }
        Self(state)
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let mut state = self.0.lock().unwrap();
        state.live = state.live.saturating_sub(1);
    }
}

/// The answer the mock gives on `GET /models`. `Default` is the normal list of
/// [`MODELS_JSON`]; a test can hand it an error, a malformed body, or a list of
/// its own.
#[derive(Clone, Debug)]
pub struct ModelsAnswer {
    pub status: u16,
    pub body: String,
    /// Held this long before answering: a slow list is a real case (a big
    /// catalogue through a proxy, STEER-6 review S2).
    pub delay: Option<Duration>,
}

impl ModelsAnswer {
    pub fn ok(body: impl Into<String>) -> ModelsAnswer {
        ModelsAnswer {
            status: 200,
            body: body.into(),
            delay: None,
        }
    }

    pub fn http(status: u16, body: impl Into<String>) -> ModelsAnswer {
        ModelsAnswer {
            status,
            body: body.into(),
            delay: None,
        }
    }

    /// The same answer, held `delay` before it is written.
    pub fn slow(mut self, delay: Duration) -> ModelsAnswer {
        self.delay = Some(delay);
        self
    }
}

impl Default for ModelsAnswer {
    fn default() -> ModelsAnswer {
        ModelsAnswer::ok(MODELS_JSON)
    }
}

pub struct Mock {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
}

impl Mock {
    pub async fn start(script: Vec<Reply>) -> Mock {
        Mock::start_with_models(script, ModelsAnswer::default()).await
    }

    /// `start`, with `GET /models` answered by `models` instead of the normal
    /// list.
    pub async fn start_with_models(script: Vec<Reply>, models: ModelsAnswer) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State {
            script: script.into(),
            requests: Vec::new(),
            models,
            models_requests: 0,
            live: 0,
            peak: 0,
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

    /// How many times a session asked `GET /models`.
    pub fn models_requests(&self) -> u32 {
        self.state.lock().unwrap().models_requests
    }

    /// The most completion requests that were ever in flight at one moment.
    /// A request is in flight from the moment the mock reads it until its
    /// (whole) answer has been written, which is what a concurrency limit is
    /// about.
    pub fn max_concurrent(&self) -> u32 {
        self.state.lock().unwrap().peak
    }
}

/// The retry knobs every test config starts with: the short 200/400 ms
/// schedule, three attempts and a five-second budget, so a test that scripts
/// failures learns the same thing (exhaustion) in about a second instead of
/// waiting out the production half hour. A test that cares about the schedule
/// passes its own values in `extra_config`, which is appended after this and
/// therefore wins.
pub const TEST_RETRY: &str = r"retry_base_ms = 200
retry_cap_ms = 400
retry_max_attempts = 3
retry_max_wait = 5";

/// The config every test run starts with: the provider and model, the test's
/// retry knobs (minus any the caller overrides in `extra_config`), then
/// `extra_config` itself, so a test's own keys win.
pub fn test_config(extra_config: &str) -> String {
    let mut out = String::from("provider = \"openrouter\"\nmodel = \"test/model\"\n");
    for line in TEST_RETRY.lines() {
        let key = line.split('=').next().unwrap_or("").trim();
        if !key.is_empty() && !extra_config.contains(key) {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str(extra_config);
    out
}

/// What the mock answers on `GET /models`: both provider shapes, and the
/// models the tests run (the default one included).
pub const MODELS_JSON: &str = r#"{"data":[
 {"id":"deepseek/deepseek-v4.1-flash-1017","context_length":1048576,
  "supported_parameters":["tools","parallel_tool_calls","reasoning"]},
 {"id":"anthropic/claude-sonnet-4.5","context_length":200000,"supported_parameters":["tools"]},
 {"id":"test/model","context_length":262144,
  "supported_parameters":["tools","parallel_tool_calls","reasoning"]},
 {"id":"go/model","limit":{"context":1000000},"capabilities":{"tools":{"supported":true}}}
]}"#;

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

    // `GET /models` is metadata every provider serves; a session asks once,
    // before its first step. It never consumes a scripted reply and is not
    // recorded in `requests`, so a test's request stream is still only
    // completions; `models_requests` counts it.
    if path.ends_with("/models") {
        let answer = {
            let mut state = state.lock().unwrap();
            state.models_requests += 1;
            state.models.clone()
        };
        if let Some(delay) = answer.delay {
            tokio::time::sleep(delay).await;
        }
        let head = format!(
            "HTTP/1.1 {} Models\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            answer.status,
            answer.body.len()
        );
        let _ = stream.write_all(head.as_bytes()).await;
        let _ = stream.write_all(answer.body.as_bytes()).await;
        let _ = stream.flush().await;
        let _ = stream.shutdown().await;
        return;
    }

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

    let _inflight = InFlight::new(state.clone());
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
        Some(Reply::GatedHttpWith(status, extra, body, gate)) => {
            gate.notified().await;
            let mut head = format!(
                "HTTP/1.1 {status} Error\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (name, value) in &extra {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            head.push_str("\r\n");
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(body.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
        Some(Reply::HttpWith(status, extra, body)) => {
            let mut head = format!(
                "HTTP/1.1 {status} Error\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (name, value) in &extra {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            head.push_str("\r\n");
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
    std::fs::write(&config, test_config(extra_config)).unwrap();

    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_kelir"));
    // Inherited `KELIR_*`/`TERM_PROGRAM` first, the test's own environment
    // second: the scrub must never undo what the test deliberately set.
    scrub_session_env(&mut command);
    command
        .args(args)
        .current_dir(&cwd)
        .env("KELIR_CONFIG", &config)
        .env("KELIR_CONFIG_DIR", temp.path().join("no-config"))
        .env("KELIR_STATE_DIR", temp.path().join("state"))
        .env("KELIR_BASE_URL", mock.base_url())
        // No real home: a run's sources are its own temp dirs, never this
        // machine's `~/.claude/skills` (a test must not depend on a desktop).
        .env("KELIR_HOME", temp.path().join("home"))
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
    /// The test's process guard: the session and everything it starts die with
    /// the test (AGENTS.md hard rule).
    pub guard: Arc<TreeGuard>,
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
        Self::start_with_flags(mock, args, extra_config, 0).await
    }

    /// A session on a state directory a previous one already used: the second
    /// start of a box whose metadata cache is warm (STEER-1 review).
    pub async fn start_reusing(
        state: PathBuf,
        mock: &Mock,
        args: &[&str],
        extra_config: &str,
    ) -> LiveSession {
        Self::start_in(Some(state), mock, args, extra_config, 0).await
    }

    /// The same, with creation flags of the caller's own: a test that needs a
    /// session without a console (a tool shell, a service) passes
    /// `DETACHED_PROCESS` (M4 review round 2, G3).
    pub async fn start_with_flags(
        mock: &Mock,
        args: &[&str],
        extra_config: &str,
        extra_flags: u32,
    ) -> LiveSession {
        Self::start_in(None, mock, args, extra_config, extra_flags).await
    }

    async fn start_in(
        reuse_state: Option<PathBuf>,
        mock: &Mock,
        args: &[&str],
        extra_config: &str,
        extra_flags: u32,
    ) -> LiveSession {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        let config = temp.path().join("config.toml");
        std::fs::write(&config, test_config(extra_config)).unwrap();
        let state = reuse_state.unwrap_or_else(|| temp.path().join("state"));

        let mut command = live_command(&cwd, &config, &state, &mock.base_url());
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let guard = Arc::new(TreeGuard::new("live session"));
        let mut child = guard
            .spawn_with(&mut command, extra_flags)
            .await
            .expect("spawn kelir");
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
            guard,
        };
        let first = session.next_event().await;
        // A fresh session starts with `session_started`; a resume picks the
        // log up with `resumed`, and its id is the one it was given.
        match first["type"].as_str() {
            Some("session_started") => {
                session.id = first["id"].as_str().expect("session id").to_string();
            }
            Some("resumed") => {
                let sessions =
                    kelir::paths::sessions_newest_first_in(&session.state.join("sessions"));
                assert_eq!(
                    sessions.len(),
                    1,
                    "a resume of one session, got {sessions:?}"
                );
                session.id = sessions[0].clone();
            }
            other => panic!("a session's first event was {other:?}: {first}"),
        }
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
        ctl_in(&self.guard, &self.state, &self.cwd, &self.config, &full).await
    }

    /// `kelir ctl latest …`: exercises session resolution.
    pub async fn ctl_latest(&self, args: &[&str]) -> Ctl {
        let mut full = vec!["latest"];
        full.extend_from_slice(args);
        ctl_in(&self.guard, &self.state, &self.cwd, &self.config, &full).await
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
        // The `end` ack and the process exiting race: the session answers from
        // its connection task, and on a busy runner the process is gone first,
        // so the socket that was accepted closes without an answer. The server
        // exits on `end` by design, so that one signature means the `end` *was*
        // honoured; every other refusal is still a failure.
        let already_gone = answer.code != 0
            && answer
                .stderr
                .contains("closed the connection without answering");
        assert!(
            answer.code == 0 || already_gone,
            "end was refused: stdout {:?} stderr {:?}",
            answer.stdout,
            answer.stderr
        );
        // A clean end may leave a promoted job running on purpose (that is the
        // M4 guarantee), and the test may still look at it. When something is
        // still alive, the guard is parked until this test *process* exits,
        // where the OS closes the job handle and kill-on-close reaps it; a test
        // that leaves nothing running is still terminated and asserted here.
        let parked = self.guard.active() > 0;
        let guard = self.guard.clone();
        let result = self.finish().await;
        if parked {
            std::mem::forget(guard);
        }
        result
    }

    /// Kill the session the way a crash or a `taskkill` would: the socket dies
    /// and the state dir stays behind, so a later `ctl` can be tried against it.
    pub async fn kill(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }

    /// Send the session a real interrupt, Unix only: `SIGINT` to the session's
    /// own pid.
    ///
    /// Windows is deliberately not here. A console control event cannot be
    /// aimed at one process: `GenerateConsoleCtrlEvent` addresses a *process
    /// group*, and a child started without `CREATE_NEW_PROCESS_GROUP` shares
    /// the test runner's group — sending Ctrl-Break would broadcast to every
    /// process on that console, the test harness and the shell that started it
    /// included (it did exactly that on 2026-11-19). The Windows side of the
    /// signal path is the handler registration itself, covered by the unit test
    /// in `src/signals.rs`; the wait that consumes it is covered here, since the
    /// `stop` verb takes the identical branch.
    #[cfg(unix)]
    pub async fn interrupt(&self) {
        let pid = self.pid();
        let seen = std::process::Command::new("kill")
            .args(["-INT", &pid.to_string()])
            .status()
            .expect("run kill");
        assert!(seen.success(), "kill -INT {pid} failed: {seen:?}");
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
    /// Every process this test starts lives in one Job Object, so a failed or
    /// panicking test cannot leave a tree behind (AGENTS.md hard rule).
    pub guard: std::sync::Arc<TreeGuard>,
}

/// Holds a test's process trees so they cannot outlive it.
///
/// The dev box is someone's desktop: a leaked `kelir`/`bash` (or a leaked
/// console window) is a real mess, and an earlier round leaked hundreds
/// (M4 review round 2, N8). On Windows every child the harness starts is
/// created suspended, put in one kill-on-close Job Object and resumed; dropping
/// the guard terminates the job and asserts nothing survived. On Unix the guard
/// carries the same contract and relies on kelir's own tree rules (tests there
/// spawn far fewer processes and no consoles).
pub struct TreeGuard {
    label: String,
    #[cfg(windows)]
    job: kelir::proc::win::Job,
    #[cfg(not(windows))]
    _private: (),
}

impl TreeGuard {
    pub fn new(label: &str) -> TreeGuard {
        #[cfg(windows)]
        {
            let name = format!("kelir-test-{}-{}", std::process::id(), kelir::bg::new_id());
            let job = kelir::proc::win::Job::create_named(&name, true)
                .unwrap_or_else(|| panic!("cannot create the test's job object {name}"));
            TreeGuard {
                label: label.to_string(),
                job,
            }
        }
        #[cfg(not(windows))]
        {
            TreeGuard {
                label: label.to_string(),
                _private: (),
            }
        }
    }

    /// Spawn a command whose whole tree stays in the guard.
    pub async fn spawn(
        &self,
        command: &mut tokio::process::Command,
    ) -> std::io::Result<tokio::process::Child> {
        self.spawn_with(command, 0).await
    }

    /// The same, with creation flags of the caller's own (`extra` is OR-ed into
    /// the guard's `CREATE_SUSPENDED`).
    pub async fn spawn_with(
        &self,
        command: &mut tokio::process::Command,
        extra: u32,
    ) -> std::io::Result<tokio::process::Child> {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command
                .as_std_mut()
                .creation_flags(extra | kelir::proc::win::CREATE_SUSPENDED);
            let child = command.spawn()?;
            if let Some(pid) = child.id() {
                self.adopt(pid);
            }
            Ok(child)
        }
        #[cfg(not(windows))]
        {
            let _ = extra;
            command.spawn()
        }
    }

    /// Spawn a synchronous command whose whole tree stays in the guard.
    pub fn spawn_sync(
        &self,
        command: &mut std::process::Command,
    ) -> std::io::Result<std::process::Child> {
        self.spawn_sync_with(command, 0)
    }

    /// The same, with creation flags of the caller's own (`extra` is OR-ed into
    /// the guard's `CREATE_SUSPENDED`).
    pub fn spawn_sync_with(
        &self,
        command: &mut std::process::Command,
        extra: u32,
    ) -> std::io::Result<std::process::Child> {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(extra | kelir::proc::win::CREATE_SUSPENDED);
            let child = command.spawn()?;
            self.adopt(child.id());
            Ok(child)
        }
        #[cfg(not(windows))]
        {
            let _ = extra;
            command.spawn()
        }
    }

    /// How many processes are still alive in the guard.
    pub fn active(&self) -> u32 {
        #[cfg(windows)]
        {
            self.job.active_processes().unwrap_or(0)
        }
        #[cfg(not(windows))]
        {
            0
        }
    }

    #[cfg(windows)]
    fn adopt(&self, pid: u32) {
        // A suspended child that cannot enter the job must not be left
        // suspended: say so loudly, then let it run outside the guard.
        match self
            .job
            .assign(pid)
            .and_then(|()| kelir::proc::win::resume(pid).map_err(|error| error.to_string()))
        {
            Ok(()) => {}
            Err(error) => {
                eprintln!(
                    "test harness: cannot contain pid {pid} in {}: {error}",
                    self.label
                );
                let _ = kelir::proc::win::resume(pid);
            }
        }
    }
}

impl Drop for TreeGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            self.job.terminate_job();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while let Some(active) = self.job.active_processes() {
                if active == 0 {
                    return;
                }
                if std::time::Instant::now() >= deadline {
                    let message = format!(
                        "the test leaked {active} process(es) in {} — kill them by pid",
                        self.label
                    );
                    if std::thread::panicking() {
                        eprintln!("LEAK: {message}");
                    } else {
                        panic!("{message}");
                    }
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
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
        std::fs::write(&config, test_config(extra_config)).unwrap();
        let state = temp.path().join("state");
        Sandbox {
            _temp: Some(temp),
            cwd,
            config,
            state,
            guard: std::sync::Arc::new(TreeGuard::new("sandbox")),
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
            guard: live.guard.clone(),
        }
    }

    /// Run the real binary against `base_url`, capturing stdout and stderr.
    pub async fn kelir(&self, base_url: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = live_command(&self.cwd, &self.config, &self.state, base_url);
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let child = self.guard.spawn(&mut command).await.expect("run kelir");
        let output = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
            .await
            .expect("kelir did not finish within 60 s")
            .expect("run kelir");
        Output {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// A synchronous command with this sandbox's environment: for a test that
    /// has to create the process with flags of its own (a console-less parent).
    pub fn kelir_command(&self, base_url: &str) -> std::process::Command {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_kelir"));
        scrub_session_env_std(&mut command);
        command
            .current_dir(&self.cwd)
            .env("KELIR_CONFIG", &self.config)
            .env("KELIR_CONFIG_DIR", self.state.join("no-config"))
            .env("KELIR_STATE_DIR", &self.state)
            .env("KELIR_BASE_URL", base_url)
            .env("KELIR_HOME", self.state.join("home"))
            .env("OPENROUTER_API_KEY", "test-openrouter-key")
            .env("KELIR_GO_API_KEY", "test-go-key");
        command
    }

    /// Cancel a job from synchronous code — a test's `Drop` — so a failing
    /// test cannot leak an infinite job and its shim (M4 review round 2, N8).
    pub fn cancel_now(&self, id: &str) {
        let mut command = self.kelir_command("http://127.0.0.1:9/v1");
        let _ = self
            .guard
            .spawn_sync(
                command
                    .args(["bg", "cancel", id])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            )
            .and_then(|mut child| child.wait());
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

/// The variables a child kelir must not inherit from the kelir session (or the
/// shell) that runs the suite.
///
/// `KELIR_SESSION_DIR`/`KELIR_SOCK`/`KELIR_CALL_ID`/`KELIR_BIN` are what kelir
/// exports to a bash child: kelir dogfoods itself, so a suite run from inside a
/// kelir session would otherwise hand its own `bg/` directory and socket to the
/// child, and `bg out` would read *that* session's jobs (M4 review).
/// `KELIR_PROMOTE_AFTER` would silently change promotion timing, `KELIR_HERDR`
/// and `TERM_PROGRAM` would make the binary think it is in a pane, and
/// `KELIR_CRASH_AT`/`KELIR_TEST_PANIC` are test hooks (M5-A fixes N2). The rest
/// of `KELIR_*` is removed as well, by prefix, so a new variable cannot quietly
/// leak in. `HC_TERMINAL_ID` (the Herdr marker) goes too.
fn scrubbed_keys() -> Vec<String> {
    let mut keys: Vec<String> = std::env::vars_os()
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .filter(|key| key.starts_with("KELIR_"))
        .collect();
    for key in ["TERM_PROGRAM", "HC_TERMINAL_ID"] {
        keys.push(key.to_string());
    }
    keys.sort();
    keys.dedup();
    keys
}

fn scrub_session_env(command: &mut tokio::process::Command) {
    for key in scrubbed_keys() {
        command.env_remove(key);
    }
}

fn scrub_session_env_std(command: &mut std::process::Command) {
    for key in scrubbed_keys() {
        command.env_remove(key);
    }
}

/// The same scrub for a command a test builds itself (either flavour).
pub fn scrub_std(command: &mut std::process::Command) {
    scrub_session_env_std(command);
}

pub fn scrub_tokio(command: &mut tokio::process::Command) {
    scrub_session_env(command);
}

/// The `portable_pty` flavour of the same scrub, for the TUI tests (which
/// spawn through a pty rather than a `Command`).
pub fn scrub_pty(command: &mut portable_pty::CommandBuilder) {
    for key in scrubbed_keys() {
        command.env_remove(key);
    }
}

fn live_command(
    cwd: &PathBuf,
    config: &PathBuf,
    state: &PathBuf,
    base_url: &str,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_kelir"));
    scrub_session_env(&mut command);
    command
        .current_dir(cwd)
        .env("KELIR_CONFIG", config)
        .env("KELIR_CONFIG_DIR", state.join("no-config"))
        .env("KELIR_STATE_DIR", state)
        .env("KELIR_BASE_URL", base_url)
        .env("KELIR_HOME", state.join("home"))
        .env("OPENROUTER_API_KEY", "test-openrouter-key")
        .env("KELIR_GO_API_KEY", "test-go-key");
    command
}

/// `kelir ctl` against a state dir, with no live session in the test harness.
pub async fn ctl_in(
    guard: &TreeGuard,
    state: &Path,
    cwd: &Path,
    config: &Path,
    args: &[&str],
) -> Ctl {
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
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = guard.spawn(&mut command).await.expect("spawn ctl");
    let output = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
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
