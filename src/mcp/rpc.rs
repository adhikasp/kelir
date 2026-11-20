//! The MCP client: the small JSON-RPC subset kelir needs, over a child's stdio.
//!
//! Hand-rolled instead of `rmcp` (docs/DEVIATIONS.md): `initialize`,
//! `tools/list`, `tools/call`, `notifications/cancelled`, a `ping` answer and
//! forwarding of inbound notifications are all there is. The transport is
//! newline-delimited JSON, one message per line.
//!
//! The server is a child in its own process group (Unix) or Job Object
//! (Windows, kill-on-close), exactly like a bash call's tree: killing it takes
//! its children too, and a session that dies takes it along.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot};

use super::config::ServerConfig;
use crate::proc::ProcessGroup;

/// The protocol revision kelir asks for. A server may answer with another;
/// the subset used here is the same in all of them.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// How long `initialize` may take (a cold `npx` can download first).
pub const INIT_TIMEOUT: Duration = Duration::from_secs(60);
/// How much of a server's stderr is kept to explain a failure.
const STDERR_KEEP: usize = 2048;

/// Why a request did not produce a result.
#[derive(Debug, Clone, PartialEq)]
pub enum CallError {
    /// The server could not be started, or went away.
    Unavailable(String),
    /// No answer in time. The call may or may not have happened.
    Timeout,
    /// The server answered with a JSON-RPC error.
    Rpc { code: i64, message: String },
}

impl CallError {
    pub fn message(&self) -> String {
        match self {
            CallError::Unavailable(message) => message.clone(),
            CallError::Timeout => "no answer in time (the outcome is unknown)".into(),
            CallError::Rpc { code, message } => format!("server error {code}: {message}"),
        }
    }
}

/// An inbound notification (a channel message, a progress report…).
#[derive(Debug, Clone)]
pub struct Notification {
    pub server: String,
    pub method: String,
    pub params: Value,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, CallError>>>>>;

pub struct Client {
    name: String,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    pending: Pending,
    next_id: AtomicU64,
    alive: Arc<AtomicBool>,
    stderr_tail: Arc<Mutex<String>>,
    /// Set by the reader on `notifications/tools/list_changed`.
    tools_changed: Arc<AtomicBool>,
    group: ProcessGroup,
    // Held so `kill_on_drop` fires with the client.
    _child: Mutex<Child>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.group.kill();
    }
}

/// Where `command` really is. Windows resolves `.exe` only, but MCP servers
/// are routinely `npx`/`uvx`-style `.cmd` shims, so the PATH is searched for
/// those too.
pub fn resolve_command(command: &str) -> PathBuf {
    let direct = Path::new(command);
    if direct.components().count() > 1 || direct.is_absolute() || !cfg!(windows) {
        return direct.to_path_buf();
    }
    if direct.extension().is_some() {
        return direct.to_path_buf();
    }
    let path = crate::envs::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        for ext in ["exe", "cmd", "bat"] {
            let candidate = dir.join(format!("{command}.{ext}"));
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    direct.to_path_buf()
}

/// Variables a server inherits from the session (matched without regard to
/// case, since Windows has none). Everything else, `KELIR_*` and provider keys
/// included, is withheld.
const INHERITED: &[&str] = &[
    "PATH",
    "PATHEXT",
    "HOME",
    "USER",
    "USERNAME",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LANGUAGE",
    "TERM",
    "TMPDIR",
    "TMP",
    "TEMP",
    "TZ",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
    "COMMONPROGRAMFILES",
    "COMMONPROGRAMFILES(X86)",
    "HOMEDRIVE",
    "HOMEPATH",
    "OS",
    "PROCESSOR_ARCHITECTURE",
    "NUMBER_OF_PROCESSORS",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_RUNTIME_DIR",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
];

fn inherited(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.starts_with("LC_") || INHERITED.contains(&upper.as_str())
}

impl Client {
    /// Start the server and complete the `initialize` handshake.
    ///
    /// `label` names the Windows Job Object (unique per start). `notify`
    /// receives every inbound notification.
    pub async fn start(
        config: &ServerConfig,
        cwd: &Path,
        label: &str,
        notify: Option<mpsc::UnboundedSender<Notification>>,
    ) -> Result<Client, CallError> {
        let mut command = Command::new(resolve_command(&config.command));
        command
            .args(&config.args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // The server gets a small allow-list of the session's environment, not
        // all of it: a third-party `npx` package must not receive the provider
        // key (`KELIR_*`), the session's socket, or anything else ambient.
        // What it needs beyond that goes in its `env` table.
        command.env_clear();
        for (key, value) in crate::envs::all_os() {
            if inherited(&key.to_string_lossy()) {
                command.env(key, value);
            }
        }
        for (key, value) in &config.env {
            command.env(key, value);
        }
        let group = ProcessGroup::new();
        #[cfg(windows)]
        let group = {
            let job = crate::proc::win::Job::create_named(label, true).ok_or_else(|| {
                CallError::Unavailable(format!(
                    "{}: cannot create a job object for the server",
                    config.name
                ))
            })?;
            group.with_job(job)
        };
        #[cfg(not(windows))]
        let _ = label;
        group.configure(&mut command);
        let mut child = command.spawn().map_err(|error| {
            CallError::Unavailable(format!(
                "{}: cannot start `{}`: {error}",
                config.name, config.command
            ))
        })?;
        let Some(pid) = child.id() else {
            return Err(CallError::Unavailable(format!(
                "{}: the server exited at once",
                config.name
            )));
        };
        if let Err(error) = group.spawned(pid) {
            let _ = child.kill().await;
            return Err(CallError::Unavailable(format!("{}: {error}", config.name)));
        }
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        let stderr_closed = Arc::new(AtomicBool::new(false));
        let tools_changed = Arc::new(AtomicBool::new(false));
        tokio::spawn(drain_stderr(
            stderr,
            stderr_tail.clone(),
            stderr_closed.clone(),
        ));
        let client = Client {
            name: config.name.clone(),
            stdin: Arc::new(tokio::sync::Mutex::new(stdin)),
            pending: pending.clone(),
            next_id: AtomicU64::new(1),
            alive: alive.clone(),
            stderr_tail: stderr_tail.clone(),
            tools_changed: tools_changed.clone(),
            group,
            _child: Mutex::new(child),
        };
        let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<Value>();
        tokio::spawn(read_loop(ReadLoop {
            name: config.name.clone(),
            stdout,
            pending,
            alive,
            notify,
            replies: reply_tx,
            stderr_tail,
            stderr_closed,
            tools_changed,
        }));
        // Answers to the server's own requests (`ping`) go out from here, so
        // the reader never needs the writer.
        let stdin = client.stdin.clone();
        tokio::spawn(async move {
            while let Some(message) = reply_rx.recv().await {
                let mut line = message.to_string();
                line.push('\n');
                let mut stdin = stdin.lock().await;
                if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                    return;
                }
            }
        });

        client.handshake().await?;
        Ok(client)
    }

    async fn handshake(&self) -> Result<(), CallError> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "kelir", "version": crate::VERSION},
        });
        self.request("initialize", params, INIT_TIMEOUT)
            .await
            .map_err(|error| match error {
                CallError::Timeout => CallError::Unavailable(format!(
                    "{}: the server did not answer `initialize` in {} s{}",
                    self.name,
                    INIT_TIMEOUT.as_secs(),
                    self.stderr_hint()
                )),
                CallError::Unavailable(message) => CallError::Unavailable(message),
                CallError::Rpc { code, message } => CallError::Unavailable(format!(
                    "{}: initialize failed ({code}): {message}",
                    self.name
                )),
            })?;
        self.notify("notifications/initialized", json!({})).await
    }

    /// The last of the server's stderr, as a suffix for an error message.
    pub fn stderr_hint(&self) -> String {
        hint_from(&self.stderr_tail)
    }

    /// Did the server announce `notifications/tools/list_changed` since the
    /// last call? (Reading clears it.)
    pub fn take_tools_changed(&self) -> bool {
        self.tools_changed.swap(false, Ordering::SeqCst)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Kill the server's whole tree.
    pub fn kill(&self) {
        self.alive.store(false, Ordering::SeqCst);
        self.group.kill();
    }

    async fn send(&self, message: &Value) -> Result<(), CallError> {
        let mut line = message.to_string();
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        let written = async {
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await
        }
        .await;
        written.map_err(|error| {
            CallError::Unavailable(format!(
                "{}: cannot write to the server: {error}{}",
                self.name,
                self.stderr_hint()
            ))
        })
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), CallError> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await
    }

    /// One request and its answer. A timeout sends `notifications/cancelled`
    /// and leaves the server running; the late answer, if any, is dropped.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, CallError> {
        if !self.is_alive() {
            return Err(CallError::Unavailable(format!(
                "{}: the server is not running{}",
                self.name,
                self.stderr_hint()
            )));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(id, tx);
        }
        // The reader clears `alive` before it drains the waiters, so checking
        // after the insert cannot miss a death: either the drain finds this
        // entry or `alive` is already false.
        if !self.is_alive() {
            self.forget(id);
            return Err(CallError::Unavailable(format!(
                "{}: the server exited{}",
                self.name,
                self.stderr_hint()
            )));
        }
        let sent = self
            .send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        if let Err(error) = sent {
            self.forget(id);
            return Err(error);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(CallError::Unavailable(format!(
                "{}: the server went away{}",
                self.name,
                self.stderr_hint()
            ))),
            Err(_) => {
                self.forget(id);
                let _ = self
                    .notify(
                        "notifications/cancelled",
                        json!({"requestId": id, "reason": "kelir timeout"}),
                    )
                    .await;
                Err(CallError::Timeout)
            }
        }
    }

    fn forget(&self, id: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        }
    }

    /// Every tool, following `nextCursor`.
    pub async fn list_tools(&self, timeout: Duration) -> Result<Vec<Value>, CallError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..100 {
            let params = match &cursor {
                Some(cursor) => json!({"cursor": cursor}),
                None => json!({}),
            };
            let result = self.request("tools/list", params, timeout).await?;
            if let Some(page) = result.get("tools").and_then(Value::as_array) {
                tools.extend(page.iter().cloned());
            }
            match result.get("nextCursor").and_then(Value::as_str) {
                Some(next) if !next.is_empty() => cursor = Some(next.to_string()),
                _ => break,
            }
        }
        Ok(tools)
    }

    pub async fn call_tool(
        &self,
        tool: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<Value, CallError> {
        self.request(
            "tools/call",
            json!({"name": tool, "arguments": arguments}),
            timeout,
        )
        .await
    }
}

fn hint_from(tail: &Mutex<String>) -> String {
    let tail = tail
        .lock()
        .map(|tail| tail.trim().to_string())
        .unwrap_or_default();
    if tail.is_empty() {
        return String::new();
    }
    let one_line: String = tail
        .lines()
        .last()
        .unwrap_or("")
        .chars()
        .take(300)
        .collect();
    format!(" (server stderr: {one_line})")
}

async fn drain_stderr(
    mut stderr: tokio::process::ChildStderr,
    tail: Arc<Mutex<String>>,
    closed: Arc<AtomicBool>,
) {
    let mut buffer = [0u8; 1024];
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let text = String::from_utf8_lossy(&buffer[..n]).into_owned();
                if let Ok(mut tail) = tail.lock() {
                    tail.push_str(&text);
                    if tail.len() > STDERR_KEEP {
                        let mut cut = tail.len() - STDERR_KEEP;
                        while !tail.is_char_boundary(cut) {
                            cut += 1;
                        }
                        tail.drain(..cut);
                    }
                }
            }
        }
    }
    closed.store(true, Ordering::SeqCst);
}

struct ReadLoop {
    name: String,
    stdout: tokio::process::ChildStdout,
    pending: Pending,
    alive: Arc<AtomicBool>,
    notify: Option<mpsc::UnboundedSender<Notification>>,
    replies: mpsc::UnboundedSender<Value>,
    stderr_tail: Arc<Mutex<String>>,
    stderr_closed: Arc<AtomicBool>,
    tools_changed: Arc<AtomicBool>,
}

async fn read_loop(state: ReadLoop) {
    let ReadLoop {
        name,
        stdout,
        pending,
        alive,
        notify,
        replies,
        stderr_tail,
        stderr_closed,
        tools_changed,
    } = state;
    let mut lines = BufReader::new(stdout);
    let mut raw = Vec::new();
    loop {
        raw.clear();
        // Bytes, not `lines()`: a line that is not UTF-8 (a stray byte in a
        // log line) is junk to skip, not the end of the connection.
        match lines.read_until(b'\n', &mut raw).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&raw);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            // A server that prints a banner on stdout is not speaking MCP on
            // that line; skip it rather than die.
            continue;
        };
        let id = message.get("id").cloned();
        let method = message.get("method").and_then(Value::as_str);
        match (id, method) {
            // A response to one of ours.
            (Some(id), None) => {
                let Some(id) = id.as_u64() else { continue };
                let waiter = pending.lock().ok().and_then(|mut map| map.remove(&id));
                let Some(waiter) = waiter else { continue };
                let outcome = if let Some(error) = message.get("error") {
                    Err(CallError::Rpc {
                        code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                        message: error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    })
                } else {
                    Ok(message.get("result").cloned().unwrap_or(Value::Null))
                };
                let _ = waiter.send(outcome);
            }
            // A request from the server: only `ping` is understood.
            (Some(id), Some(method)) => {
                let reply = if method == "ping" {
                    json!({"jsonrpc": "2.0", "id": id, "result": {}})
                } else {
                    json!({"jsonrpc": "2.0", "id": id,
                           "error": {"code": -32601, "message": "method not found"}})
                };
                let _ = replies.send(reply);
            }
            (None, Some(method)) => {
                if method == "notifications/tools/list_changed" {
                    tools_changed.store(true, Ordering::SeqCst);
                }
                if let Some(notify) = &notify {
                    let _ = notify.send(Notification {
                        server: name.clone(),
                        method: method.to_string(),
                        params: message.get("params").cloned().unwrap_or(Value::Null),
                    });
                }
            }
            (None, None) => {}
        }
    }
    // EOF: the server exited. Everyone still waiting learns it now.
    alive.store(false, Ordering::SeqCst);
    // The reason is on stderr, which may still be draining: give it a moment.
    for _ in 0..20 {
        if stderr_closed.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let hint = hint_from(&stderr_tail);
    let waiters: Vec<_> = pending
        .lock()
        .map(|mut map| map.drain().collect())
        .unwrap_or_default();
    for (_, waiter) in waiters {
        let _ = waiter.send(Err(CallError::Unavailable(format!(
            "{name}: the server exited{hint}"
        ))));
    }
}
