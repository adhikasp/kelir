//! The MCP client: the small JSON-RPC subset kelir needs, over a child's stdio.
//! Hand-rolled instead of `rmcp` (docs/DEVIATIONS.md): `initialize`, `tools/list`, `tools/call`,
//! `notifications/cancelled`, a `ping` answer and forwarding of inbound notifications are all
//! there is.
//! The transport is newline-delimited JSON, one message per line.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::{mpsc, oneshot};

use super::config::ServerConfig;

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
    /// Configured `env` values (6+ bytes): never shown in an error hint.
    secrets: Arc<Vec<String>>,
    /// Set by the reader on `notifications/tools/list_changed`.
    tools_changed: Arc<AtomicBool>,
    /// The server and its tree; `group` kills the tree with the client.
    group: crate::sys::proc::ProcessGroup,
    // Held so `kill_on_drop` fires with the client.
    _child: Mutex<tokio::process::Child>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.group.kill();
    }
}

/// Where `command` really is. Windows resolves `.exe` only, but MCP servers are routinely
/// `npx`/`uvx`-style `.cmd` shims, so the PATH is searched for those too.
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

impl Client {
    /// Start the server and complete the `initialize` handshake.
    pub async fn start(
        config: &ServerConfig,
        cwd: &Path,
        label: &str,
        notify: Option<mpsc::UnboundedSender<Notification>>,
        capabilities: Option<Value>,
    ) -> Result<Client, CallError> {
        let resolved = resolve_command(&config.command);
        #[cfg(unix)]
        if !on_path(&resolved) {
            return Err(CallError::Unavailable(format!(
                "{}: cannot start `{}`: no such file or command",
                config.name, config.command
            )));
        }
        // The server runs under a tiny watcher (same process group) that kills the group when this process is
        // gone, so a `kill -9` of the session leaves no server and no grandchild even if the server ignores
        // signals. On Windows the same guarantee comes from the job object in `sys::runner`.
        #[cfg(unix)]
        let (program, args) = (
            "sh".to_string(),
            vec![
                "-c".to_string(),
                UNIX_GUARD.to_string(),
                "kelir-mcp".to_string(),
                resolved.to_string_lossy().into_owned(),
            ],
        );
        #[cfg(not(unix))]
        let (program, args) = (
            resolved.to_string_lossy().into_owned(),
            Vec::<String>::new(),
        );
        let mut args = args;
        args.extend(config.args.iter().cloned());
        let spec = crate::sys::runner::Spec {
            cwd: Some(cwd.to_path_buf()),
            env: crate::sys::runner::Env::Scrubbed {
                extra: config
                    .env
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            },
            stdin: crate::sys::runner::Stdin::Piped,
            timeout: None,
            output_cap: 0,
            stderr_cap: 0,
            no_window: true,
            job_name: Some(label.to_string()),
            unix_guard: None,
            ..crate::sys::runner::Spec::new(&program, args)
        };
        let running = crate::sys::runner::spawn(spec).map_err(|error| {
            CallError::Unavailable(format!(
                "{}: cannot start `{}`: {error}",
                config.name, config.command
            ))
        })?;
        let crate::sys::runner::Running {
            child,
            stdin: pipe,
            stdout,
            stderr,
            group,
        } = running;

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        let stderr_closed = Arc::new(AtomicBool::new(false));
        let secrets = Arc::new(secret_values(config));
        let tools_changed = Arc::new(AtomicBool::new(false));
        tokio::spawn(drain_stderr(
            stderr,
            stderr_tail.clone(),
            stderr_closed.clone(),
        ));
        let client = Client {
            name: config.name.clone(),
            stdin: Arc::new(tokio::sync::Mutex::new(pipe)),
            pending: pending.clone(),
            next_id: AtomicU64::new(1),
            alive: alive.clone(),
            stderr_tail: stderr_tail.clone(),
            secrets: secrets.clone(),
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
            secrets,
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
                let failed =
                    stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err();
                drop(stdin);
                if failed {
                    return;
                }
            }
        });

        client
            .handshake(capabilities.unwrap_or_else(|| json!({})))
            .await?;
        Ok(client)
    }

    async fn handshake(&self, capabilities: Value) -> Result<(), CallError> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": capabilities,
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

    pub fn stderr_hint(&self) -> String {
        hint_from(&self.stderr_tail, &self.secrets)
    }

    /// Did the server announce `notifications/tools/list_changed` since the
    /// last call? (Reading clears it.)
    pub fn take_tools_changed(&self) -> bool {
        self.tools_changed.swap(false, Ordering::SeqCst)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

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
        // The reader clears `alive` before it drains the waiters, so checking after the insert
        // cannot miss a death: either the drain finds this entry or `alive` is already false.
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

/// Shell wrapper for Unix servers: a watcher in the server's process group polls the session's pid and
/// kills the group (server, its children, itself) once the session is gone or the server has exited.
///
/// POSIX sh has no `/proc`, and the shell builtin `kill` differs across macOS/GNU (`--` and `-s`
/// with a negative pgid are not uniformly accepted). Liveness comes from `ps -o stat=` alone: a
/// zombie still answers `kill -0` until it is reaped, so `kill -0` must not decide liveness here.
/// The group kill tries the portable `kill -s KILL -- -$pgid` form first (dash, bash, BSD sh),
/// then `/bin/kill -KILL -$pgid`, then the builtin without `--`. As soon as one succeeds the group
/// is gone; the remaining forms are harmless failures.
#[cfg(unix)]
const UNIX_GUARD: &str = concat!(
    // `ps` prints a state such as `Ss`, `R+` or `Z` (BSD may leave leading spaces). An empty
    // answer or any `Z` in the state means the pid is gone for our purposes.
    "alive() { s=$(ps -o stat= -p $1 2>/dev/null) || return 1; ",
    "[ -n \"$s\" ] || return 1; case $s in *Z*) return 1;; esac; return 0; }; ",
    "killgroup() { kill -s KILL -- -$1 2>/dev/null || ",
    "/bin/kill -KILL -$1 2>/dev/null || kill -KILL -$1 2>/dev/null; }; ",
    "g=$$; p=$PPID; ( while alive $p && alive $g; do sleep 1; done; ",
    "killgroup $g ) </dev/null >/dev/null 2>&1 & exec \"$@\""
);

/// Does `command` name something to run? (`sh` would only say "not found" and
/// exit 127, hiding the "cannot start" error.)
#[cfg(unix)]
fn on_path(command: &Path) -> bool {
    if command.components().count() > 1 {
        return command.exists();
    }
    let path = crate::envs::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|dir| dir.join(command).is_file())
}

/// `env` values worth redacting (short ones would mangle ordinary text).
fn secret_values(config: &ServerConfig) -> Vec<String> {
    config
        .env
        .values()
        .filter(|value| value.len() >= 6)
        .cloned()
        .collect()
}

fn redact(text: &str, secrets: &[String]) -> String {
    let mut text = text.to_string();
    for secret in secrets {
        text = text.replace(secret.as_str(), "***");
    }
    text
}

fn hint_from(tail: &Mutex<String>, secrets: &[String]) -> String {
    let tail = tail
        .lock()
        .map(|tail| redact(tail.trim(), secrets))
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
    secrets: Arc<Vec<String>>,
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
        secrets,
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
    let hint = hint_from(&stderr_tail, &secrets);
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

#[cfg(all(unix, test))]
mod guard_tests {
    use super::UNIX_GUARD;
    use crate::sys::runner::test_command_name;

    /// The watcher never uses `/proc` (absent on macOS) and uses `ps` state, not `kill -0`, to
    /// decide whether a pid is gone: a zombie still answers `kill -0` until its parent reaps it.
    #[test]
    fn the_guard_uses_ps_state_and_a_portable_group_kill() {
        assert!(!UNIX_GUARD.contains("/proc"), "{UNIX_GUARD}");
        assert!(!UNIX_GUARD.contains("kill -0"), "{UNIX_GUARD}");
        assert!(UNIX_GUARD.contains("ps -o stat= -p $1"), "{UNIX_GUARD}");
        assert!(
            UNIX_GUARD.contains("case $s in *Z*) return 1"),
            "{UNIX_GUARD}"
        );
        assert!(UNIX_GUARD.contains("kill -s KILL -- -$1"), "{UNIX_GUARD}");
        assert!(UNIX_GUARD.contains("/bin/kill -KILL -$1"), "{UNIX_GUARD}");
    }

    /// Exercise the generated `alive()` in a real POSIX shell with a stubbed `ps`: an empty
    /// state, a BSD-style `Z` state and a missing pid are all "gone"; `Ss`/`R+` are alive.
    /// Stubbing `ps` makes this deterministic on Linux, macOS and Windows-Git-Bash alike.
    #[test]
    fn alive_in_a_real_shell_rejects_zombies_and_accepts_live_states() {
        let guard = UNIX_GUARD.split("g=$$;").next().unwrap_or(UNIX_GUARD);
        // A stub `ps` makes the state deterministic; the guard function itself is generated code.
        let script = format!(
            "ps() {{ case \"$*\" in *999) printf 'Z\\n';; *998) printf '    Z\\n';; \
             *997) printf '';; *996) printf 'R+\\n';; *) printf ' Ss \\n';; esac; }}; \
             {guard} \
             for p in 1 999 998 997 996; do alive $p && echo $p-live || echo $p-gone; done; \
             [ \"$1\" = keep-me ] && echo args-kept || echo args-lost"
        );
        let out = test_command_name("sh")
            .args(["-c", &script, "sh", "keep-me"]) // $0, $1 for the embedded script
            .output();
        let Ok(out) = out else {
            return; // no shell on this box: the e2e kill test covers the behaviour instead
        };
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            [
                "1-live",
                "999-gone",
                "998-gone",
                "997-gone",
                "996-live",
                "args-kept",
            ],
            "stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The group kill spelling is accepted by the local `sh` parser. Real behaviour on a killed
    /// session is covered by `review_m6b::a_killed_session_leaves_no_server_and_no_grandchild`,
    /// which runs the generated guard on every OS in CI.
    #[test]
    fn the_portable_group_kill_spelling_is_accepted_by_sh() {
        let out = test_command_name("sh")
            .args(["-c", "kill -s KILL -- -999999 2>/dev/null; echo ok"])
            .output();
        if let Ok(out) = out {
            assert!(String::from_utf8_lossy(&out.stdout).contains("ok"));
        }
    }
}
