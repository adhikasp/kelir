//! Scripted in-process OpenAI-compatible SSE server for the integration tests.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::net::SocketAddr;
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
    /// The body exactly as received (`body` is it parsed): for tests that
    /// compare bytes.
    pub raw_body: String,
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

/// Picks the reply to a completion request by looking at it (M7: several
/// sessions share one mock and their requests interleave, so a FIFO script
/// cannot say who gets what). `None` falls back to the script.
pub type Router = Arc<dyn Fn(&Recorded) -> Option<Reply> + Send + Sync>;

struct State {
    router: Option<Router>,
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
    /// Held until the test notifies this gate (STEER-9): "slower than the turn"
    /// as an ordering the test controls, not as a number of seconds a loaded
    /// box can overtake.
    pub gate: Option<Arc<tokio::sync::Notify>>,
}

impl ModelsAnswer {
    pub fn ok(body: impl Into<String>) -> ModelsAnswer {
        ModelsAnswer {
            status: 200,
            body: body.into(),
            delay: None,
            gate: None,
        }
    }

    pub fn http(status: u16, body: impl Into<String>) -> ModelsAnswer {
        ModelsAnswer {
            status,
            body: body.into(),
            delay: None,
            gate: None,
        }
    }

    /// The same answer, held until `gate` is notified.
    pub fn gated(mut self, gate: Arc<tokio::sync::Notify>) -> ModelsAnswer {
        self.gate = Some(gate);
        self
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
    /// A mock whose replies are chosen per request by `router`; a request it
    /// declines gets the next scripted reply.
    pub async fn start_routed(
        script: Vec<Reply>,
        router: impl Fn(&Recorded) -> Option<Reply> + Send + Sync + 'static,
    ) -> Mock {
        let mock = Mock::start(script).await;
        mock.state.lock().unwrap().router = Some(Arc::new(router));
        mock
    }

    pub async fn start_with_models(script: Vec<Reply>, models: ModelsAnswer) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State {
            router: None,
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
pub const TEST_RETRY: &str = r"[retry]
base_ms = 200
cap_ms = 400
max_attempts = 3
max_wait = 5";

/// The config every test run starts with: the provider and model, the test's
/// retry knobs (minus any the caller overrides in `extra_config`), then
/// `extra_config` itself, so a test's own keys win.
pub fn test_config(extra_config: &str) -> String {
    let extra = normalize_test_keys(extra_config);
    let Ok(mut base) =
        toml::from_str::<toml::Value>("provider = \"openrouter\"\nmodel = \"test/model\"\n")
    else {
        return extra;
    };
    let Ok(extra) = toml::from_str::<toml::Value>(&extra) else {
        return format!("{}{}\n{}", base, TEST_RETRY, extra);
    };
    let Ok(retry) = toml::from_str::<toml::Value>(TEST_RETRY) else {
        return format!("{base}{extra}");
    };
    merge_toml(&mut base, retry);
    merge_toml(&mut base, extra.clone());
    toml::to_string_pretty(&base).unwrap_or_else(|_| format!("{base}{extra}"))
}

fn merge_toml(base: &mut toml::Value, over: toml::Value) {
    match (base, over) {
        (base, toml::Value::Table(over)) => {
            let Some(table) = base.as_table_mut() else {
                *base = toml::Value::Table(over);
                return;
            };
            for (key, value) in over {
                match table.get_mut(&key) {
                    Some(existing) => merge_toml(existing, value),
                    None => {
                        table.insert(key, value);
                    }
                }
            }
        }
        (base, value) => *base = value,
    }
}

/// Older tests spell a grouped key flat. The harness translates it so the
/// tests keep saying what they meant; the binary itself reads only the grouped
/// spelling (no compatibility aliases in `src/config.rs`).
fn normalize_test_keys(extra: &str) -> String {
    const FLAT: &[(&str, &str, &str)] = &[
        ("auto_compact_at", "compaction", "auto_at"),
        ("compact_at_tokens", "compaction", "at_tokens"),
        ("compact_prompt", "compaction", "prompt"),
        ("compact_keep", "compaction", "keep"),
        ("retry_base_ms", "retry", "base_ms"),
        ("retry_cap_ms", "retry", "cap_ms"),
        ("retry_max_wait", "retry", "max_wait"),
        ("retry_max_attempts", "retry", "max_attempts"),
        ("max_child_depth", "children", "max_depth"),
        ("max_children", "children", "max_per_agent"),
        ("max_live_descendants", "children", "max_live_descendants"),
        ("channel_max_active", "channel", "max_active"),
        (
            "channel_per_conversation_rpm",
            "channel",
            "per_conversation_rpm",
        ),
        ("channel_global_rpm", "channel", "global_rpm"),
        ("channel_fallback", "channel", "fallback"),
        ("channel_spool_limit", "channel", "spool_limit"),
    ];
    let Ok(mut value) = toml::from_str::<toml::Value>(extra) else {
        return extra.to_string();
    };
    let Some(table) = value.as_table_mut() else {
        return extra.to_string();
    };
    for (old, group, new) in FLAT {
        let Some(entry) = table.remove(*old) else {
            continue;
        };
        let group = table
            .entry(*group)
            .or_insert_with(|| toml::Value::Table(Default::default()));
        if let Some(target) = group.as_table_mut() {
            target.insert((*new).to_string(), entry);
        }
    }
    toml::to_string_pretty(&value).unwrap_or_else(|_| extra.to_string())
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
    let raw_body = String::from_utf8_lossy(&body[..body.len().min(content_length)]).into_owned();
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
        if let Some(gate) = answer.gate {
            gate.notified().await;
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

    let (recorded, router) = {
        let mut state = state.lock().unwrap();
        let recorded = Recorded {
            method,
            path,
            headers,
            body,
            raw_body,
        };
        state.requests.push(recorded.clone());
        (recorded, state.router.clone())
    };
    let routed = router.and_then(|router| router(&recorded));
    let reply = match routed {
        Some(reply) => Some(reply),
        None => state.lock().unwrap().script.pop_front(),
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
            }
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
