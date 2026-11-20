//! The session socket: newline-delimited JSON over a Unix socket (a named pipe
//! on Windows), served by the session process.
//!
//! Request: `{"verb": "say", "args": {"text": "hi"}, "from": "ctl",
//! "token": "…"}`.
//! Answer: one line, `{"ok": true, …}` or `{"ok": false, "error": "…"}`.
//!
//! Reaching the socket is not authorisation (STEER-7): every request carries the
//! session's per-session token, and the listener answers and drops a request
//! that does not before the session sees it. See [`crate::trust`].
//!
//! The path is `<session dir>/sock` on Unix and `\\.\pipe\kelir-<session id>`
//! on Windows; both open with the same code, so `kelir ctl` never needs to know
//! which platform the session runs on. The address is also exported to every
//! bash child as `KELIR_SOCK`.
//!
//! A Unix socket path has to fit in `sun_path` (~108 bytes) and only the
//! listener may clean up a stale file, so the address is resolved once here and
//! both sides read the same answer: when `<session dir>/sock` is too long the
//! session binds a short path in `$XDG_RUNTIME_DIR` (or the temp dir) and
//! records it in `<session dir>/sock.path`.

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[cfg(not(windows))]
use interprocess::local_socket::GenericFilePath;
#[cfg(windows)]
use interprocess::local_socket::GenericNamespaced;
use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::tokio::{Listener as LocalListener, Stream as LocalStream};
use interprocess::local_socket::{ConnectOptions, ListenerOptions, Name};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

/// The socket's file name on Unix; on Windows the pipe name carries the id.
pub const SOCKET_FILE: &str = "sock";
/// The lock file that says "this session's log has a writer".
pub const LOCK_FILE: &str = "session.lock";
/// A Unix socket path must fit in `sun_path` (108 bytes on Linux, 104 on the
/// BSDs and macOS); this leaves the last byte for the terminator.
#[cfg(all(unix, any(target_os = "linux", target_os = "android")))]
const MAX_SOCKET_PATH: usize = 107;
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
const MAX_SOCKET_PATH: usize = 103;
/// Where a session records the socket path it bound when that is not
/// `<session dir>/sock` (see [`socket_path`]).
#[cfg(unix)]
pub const SOCKET_POINTER_FILE: &str = "sock.path";
/// Named-pipe prefix on Windows.
#[cfg(windows)]
const PIPE_PREFIX: &str = "kelir-";
/// How long a client waits for a connection or an answer.
pub const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest request line the listener will read. A verb's arguments are a job
/// status or a sentence; 64 KB is far past anything `kelir ctl` sends and far
/// short of what a hostile peer would like this process to hold (STEER-7 review
/// N3).
pub const MAX_LINE: usize = 64 * 1024;
/// How long a connection may sit without sending its request. A client that
/// connects and says nothing is not a client of this session; it is refused
/// rather than held open for as long as it likes (STEER-7 review N3).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(10);

/// One request line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub verb: String,
    #[serde(default)]
    pub args: Value,
    /// Who sent it: `ctl`, `tui`, `gate`…
    #[serde(default = "default_from")]
    pub from: String,
    /// The session's token ([`crate::trust::TOKEN_ENV`]). A request that crosses
    /// the socket without it is refused and dropped at the listener; the TUI and
    /// the session's own watcher run inside the session and set it from memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The **job capability** the sender holds ([`crate::trust::CAP_ENV`]): a
    /// per-job secret only that job's shim has. A `bg` report must carry it, and
    /// the session's token is not a substitute — every tool child has the token
    /// (STEER-7 review B1). Never logged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
}

fn default_from() -> String {
    "ctl".into()
}

impl Request {
    pub fn new(verb: impl Into<String>, args: Value, from: impl Into<String>) -> Self {
        Self {
            verb: verb.into(),
            args,
            from: from.into(),
            token: None,
            capability: None,
        }
    }

    /// The same request, carrying a token (what the client side does).
    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token;
        self
    }

    /// The same request, carrying a job's capability (what a shim's report does).
    pub fn with_capability(mut self, capability: Option<String>) -> Self {
        self.capability = capability.filter(|capability| !capability.trim().is_empty());
        self
    }
}

/// One answer line. Exactly one per request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verb: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// Where the verb will apply: `now`, `next_step`, `after_tools`, `turn_end`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applies: Option<String>,
    /// The session's state when it answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(verb: &str, seq: u64, applies: &str, state: Value) -> Self {
        Self {
            ok: true,
            verb: Some(verb.to_string()),
            seq: Some(seq),
            applies: Some(applies.to_string()),
            state: Some(state),
            error: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            verb: None,
            seq: None,
            applies: None,
            state: None,
            error: Some(message.into()),
        }
    }

    pub fn line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"ok":false,"error":"cannot serialise the response"}"#.to_string()
        })
    }
}

/// A request handed to the session loop, with the channel its answer travels
/// back on.
#[derive(Debug)]
pub struct Incoming {
    pub request: Request,
    pub reply: oneshot::Sender<Response>,
    /// Set when the session itself raised this wake-up (its own `bg/` watcher),
    /// which is not a request at all: there is no `Request` to answer, so the
    /// action is carried here and the loop takes it directly. `None` for
    /// everything that arrived on the socket or from the TUI (STEER-7: the
    /// watcher no longer impersonates a `job` on the wire).
    pub action: Option<Action>,
}

/// Work the session owes itself.
#[derive(Debug)]
pub enum Action {
    /// A status file in this session's `bg/` started or settled.
    Note(crate::bg::Status),
}

// The address: a path on Unix, a name on Windows.

/// True when `path` fits in `sun_path`.
#[cfg(unix)]
fn fits(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    path.as_os_str().as_bytes().len() <= MAX_SOCKET_PATH
}

/// A short place for a socket whose session dir path does not fit:
/// `$XDG_RUNTIME_DIR` (per-user, 0700) when it exists, else the temp dir. Only
/// the session id goes in the file name, so the result stays short whatever
/// the state dir looks like. The socket is chmod 0600 either way, and a shared
/// temp dir is only used when there is no runtime dir.
#[cfg(unix)]
fn fallback_path(id: &str) -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    base.join(format!("kelir-{id}.sock"))
}

/// The path a session's socket lives at.
///
/// `<session dir>/sock` is the address in every normal case. `sun_path` is
/// only ~108 bytes though, and a state dir can be deeper than that (a deep
/// home, or `KELIR_STATE_DIR` pointing somewhere long), so a session that
/// cannot use the canonical path binds a short one and records it in
/// `<session dir>/sock.path`; the record wins over any guess a client makes.
#[cfg(unix)]
fn socket_path(dir: &Path, id: &str) -> PathBuf {
    if let Ok(recorded) = std::fs::read_to_string(dir.join(SOCKET_POINTER_FILE)) {
        let recorded = PathBuf::from(recorded.trim());
        if !recorded.as_os_str().is_empty() {
            return recorded;
        }
    }
    let canonical = dir.join(SOCKET_FILE);
    if fits(&canonical) {
        canonical
    } else {
        fallback_path(id)
    }
}

/// Record the socket path when it is not the canonical one, so `kelir ctl` and
/// every bash child holding `KELIR_SOCK` open the socket that was really bound.
#[cfg(unix)]
fn record_socket_path(dir: &Path, path: &Path) {
    let pointer = dir.join(SOCKET_POINTER_FILE);
    if path == dir.join(SOCKET_FILE) {
        let _ = std::fs::remove_file(pointer);
    } else {
        let _ = std::fs::write(pointer, crate::util::path_string(path));
    }
}

/// The address of a session's socket, for `KELIR_SOCK`, error messages and the
/// client.
#[cfg(unix)]
pub fn value(dir: &Path, id: &str) -> String {
    crate::util::path_string(&socket_path(dir, id))
}
#[cfg(windows)]
pub fn value(_dir: &Path, id: &str) -> String {
    format!(r"\\.\pipe\{PIPE_PREFIX}{id}")
}

#[cfg(not(any(unix, windows)))]
pub fn value(dir: &Path, _id: &str) -> String {
    crate::util::path_string(&dir.join(SOCKET_FILE))
}

fn name(dir: &Path, id: &str) -> Result<Name<'static>, String> {
    #[cfg(unix)]
    {
        let path = socket_path(dir, id);
        if !fits(&path) {
            return Err(format!(
                "the session socket path {} is {} bytes and a Unix socket takes at most \
                 {MAX_SOCKET_PATH}; set KELIR_STATE_DIR to a shorter directory",
                crate::util::path_string(&path),
                path.as_os_str().len(),
            ));
        }
        path.to_fs_name::<GenericFilePath>()
            .map_err(|error| format!("cannot name the session socket: {error}"))
    }
    #[cfg(windows)]
    {
        let _ = dir;
        format!("{PIPE_PREFIX}{id}")
            .to_ns_name::<GenericNamespaced>()
            .map_err(|error| format!("cannot name the session pipe: {error}"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let path = dir.join(SOCKET_FILE);
        path.to_fs_name::<GenericFilePath>()
            .map_err(|error| format!("cannot name the session socket: {error}"))
    }
}

/// Exclusive ownership of one session's log, held for the life of the process.
///
/// On Unix `bind` unlinks a stale socket file before it listens, so two
/// `resume`s started at the same moment can both pass the "already running"
/// connect check and both bind: the check is connect-then-bind, not one atomic
/// step (M3 review N3). An exclusive `flock` on `<session dir>/session.lock`
/// is that step; the loser exits before it writes a byte. On Windows the named
/// pipe's first-instance rule already makes `bind` the atomic test, so the
/// guard carries no file there.
pub struct SessionLock {
    #[cfg(unix)]
    _file: std::fs::File,
}

/// Take the session's write lock, or say who else has it.
#[cfg(unix)]
pub fn lock(dir: &Path) -> Result<SessionLock, String> {
    use std::os::unix::io::AsRawFd as _;
    let path = dir.join(LOCK_FILE);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        // The lock file is empty; opening it must not truncate another
        // process's lock while it holds one.
        .truncate(false)
        .open(&path)
        .map_err(|error| {
            format!(
                "cannot open the lock file {}: {error}",
                crate::util::path_string(&path)
            )
        })?;
    // SAFETY: `file` owns a valid descriptor for as long as the guard lives.
    let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if taken != 0 {
        return Err("another process is writing its log".to_string());
    }
    Ok(SessionLock { _file: file })
}

#[cfg(not(unix))]
pub fn lock(_dir: &Path) -> Result<SessionLock, String> {
    Ok(SessionLock {})
}

/// Bind the listener. Done before `session_started` is recorded, so a client
/// that sees the event can connect.
pub fn bind(dir: &Path, id: &str) -> Result<LocalListener, String> {
    #[cfg(unix)]
    let path = socket_path(dir, id);
    // Only the listener cleans up: a stale file from a session that died without
    // cleaning up would make bind fail, and it cannot be a live listener, since
    // the session dir is new. A *client* must never unlink the path — doing that
    // is what made every round trip fail on Linux.
    #[cfg(unix)]
    let _ = std::fs::remove_file(&path);
    let address = name(dir, id)?;
    #[cfg(windows)]
    let options = {
        // An explicit descriptor, not the platform default: a protected DACL
        // with one ACE for this process's user (STEER-7). `create_tokio` copies
        // it into the pipe, so the descriptor is released here.
        use interprocess::os::windows::local_socket::ListenerOptionsExt as _;
        let descriptor = crate::perm::pipe_descriptor()
            .map_err(|error| format!("cannot build the session pipe's DACL: {error}"))?;
        ListenerOptions::new()
            .name(address)
            .security_descriptor(descriptor)
    };
    #[cfg(not(windows))]
    let options = ListenerOptions::new().name(address);
    let listener = options.create_tokio().map_err(|error| {
        format!(
            "cannot listen on the session socket {}: {error}",
            value(dir, id)
        )
    })?;
    // `say` is a prompt-injection channel into a fully autonomous agent, so the
    // socket is the user's alone: 0600 on Unix, an owner-only pipe DACL on
    // Windows. Both are *re-checked* after the fact — a umask, a stale file or a
    // platform default that leaked in must fail the start, not warn (STEER-7).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        crate::perm::check_socket(&path)?;
        record_socket_path(dir, &path);
    }
    #[cfg(windows)]
    crate::perm::check_pipe(&value(dir, id))?;
    Ok(listener)
}

/// Accept connections forever. Every connection gets its own task; answers go
/// back on each request's own channel, so two clients never block each other.
///
/// The token check is here, at the boundary: a request without this session's
/// token is answered and its connection closed before it reaches `tx`, so an
/// unauthenticated caller cannot make the session do anything — or write a
/// `verb` event (STEER-7).
pub fn serve(
    listener: LocalListener,
    trust: Arc<crate::trust::Trust>,
    tx: mpsc::UnboundedSender<Incoming>,
) {
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok(stream) => {
                    let tx = tx.clone();
                    let trust = trust.clone();
                    tokio::spawn(async move { connection(stream, trust, tx).await });
                }
                // The listener only fails for good reasons (the pipe name was
                // taken, the runtime is going away); retry so one bad accept
                // cannot stop the session from being reachable.
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    });
}

async fn connection(
    stream: LocalStream,
    trust: Arc<crate::trust::Trust>,
    tx: mpsc::UnboundedSender<Incoming>,
) {
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read);
    let mut line = String::new();
    loop {
        line.clear();
        match read_bounded(&mut lines, &mut line, IDLE_TIMEOUT).await {
            Read::Line => {}
            // The peer is gone, said nothing in time, or sent a line nobody
            // would send: answer that once (when there is someone to answer)
            // and close (STEER-7 review N3).
            Read::Closed => return,
            Read::Refused(message) => {
                let mut out = Response::error(message).line();
                out.push('\n');
                let _ = reply_line(&mut write, &out).await;
                return;
            }
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut close = false;
        let response = match serde_json::from_str::<Request>(trimmed) {
            Ok(request) => match trust.accepts(&request) {
                Ok(()) => {
                    let (reply, answer) = oneshot::channel();
                    if tx
                        .send(Incoming {
                            request,
                            reply,
                            action: None,
                        })
                        .is_err()
                    {
                        let mut out = Response::error("the session is ending").line();
                        out.push('\n');
                        let _ = reply_line(&mut write, &out).await;
                        return;
                    }
                    match answer.await {
                        Ok(response) => response,
                        Err(_) => Response::error("the session did not answer"),
                    }
                }
                // An unauthenticated caller gets one answer and no more: it
                // is not a client of this session.
                Err(error) => {
                    close = true;
                    Response::error(error)
                }
            },
            Err(error) => Response::error(format!("bad request line: {error}")),
        };
        let mut out = response.line();
        out.push('\n');
        if !reply_line(&mut write, &out).await {
            return;
        }
        if close {
            return;
        }
    }
}

/// Write one answer line, bounded in time. An answer nobody reads is an answer
/// the peer is not waiting for (a hostile client can fill its own receive
/// buffer and then not read, which would park this connection's task for as
/// long as it likes); a write that does not finish in [`IO_TIMEOUT`] closes the
/// connection instead (STEER-7 review N3). Whether the line went out.
async fn reply_line<W>(write: &mut W, out: &str) -> bool
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt as _;
    let sent = tokio::time::timeout(IO_TIMEOUT, async {
        write.write_all(out.as_bytes()).await?;
        write.flush().await
    })
    .await;
    matches!(sent, Ok(Ok(())))
}

/// What one bounded read of a request line produced.
enum Read {
    /// A whole line is in the buffer.
    Line,
    /// The peer closed without a line (a client that just connects and leaves).
    Closed,
    /// A line the listener will not read: too long, or not sent in time. The
    /// string is what the peer is told before the connection closes.
    Refused(String),
}

/// Read one request line, bounded in **length** and in **time**: at most
/// [`MAX_LINE`] bytes are pulled in (so a megabyte with no newline is never
/// held) and at most `idle` passes before the peer is told to go away. Both
/// are what makes the listener unattractive to a hostile peer that never
/// authenticates (STEER-7 review N3).
async fn read_bounded<R>(lines: &mut BufReader<R>, line: &mut String, idle: Duration) -> Read
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncBufReadExt as _;
    use tokio::io::AsyncReadExt as _;
    // `take` limits the read half, so the limit is on bytes, not on lines.
    let mut limited = lines.take(MAX_LINE as u64);
    let read = match tokio::time::timeout(idle, limited.read_line(line)).await {
        Ok(Ok(read)) => read,
        // A read error is a peer that left: nothing to answer.
        Ok(Err(_)) => return Read::Closed,
        // Nothing in time: the peer is told once and the connection closes.
        Err(_) => {
            return Read::Refused(format!(
                "no request line within {}s; closing",
                idle.as_secs_f32().round()
            ));
        }
    };
    if read == 0 {
        return Read::Closed;
    }
    // `take` stopped at the cap without a newline: the line is too long and its
    // tail is still coming. One refusal and a close is the whole answer.
    if read >= MAX_LINE && !line.ends_with('\n') {
        return Read::Refused(format!(
            "the request line is longer than {MAX_LINE} bytes; closing"
        ));
    }
    Read::Line
}

/// Connect to a session's socket. Read-only as far as the filesystem goes: the
/// listener owns the path, so a client never creates or removes it.
pub async fn connect(dir: &Path, id: &str) -> Result<LocalStream, String> {
    let address = name(dir, id)?;
    let options = ConnectOptions::new().name(address);
    let connect = options.connect_tokio();
    match tokio::time::timeout(IO_TIMEOUT, connect).await {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("connecting timed out".to_string()),
    }
}

/// One request/answer round trip, for `kelir ctl`.
pub async fn request(dir: &Path, id: &str, request: &Request) -> Result<Response, String> {
    connect_and_send(dir, id, request).await
}

/// The same round trip, with the session's token read from its own directory
/// (`kelir ctl` from a human's terminal: it knows which session it sent to, so
/// it reads that session's `<session dir>/token` instead of the environment).
pub async fn request_with_token_file(
    dir: &Path,
    id: &str,
    request: &Request,
) -> Result<Response, String> {
    let sent = match &request.token {
        Some(_) => request.clone(),
        None => request.clone().with_token(crate::trust::read_token(dir)),
    };
    connect_and_send(dir, id, &sent).await
}

/// The body of [`request`], for the wrappers that add a token first (and so
/// cannot call it without shadowing their own `request` argument).
async fn connect_and_send(dir: &Path, id: &str, request: &Request) -> Result<Response, String> {
    let stream = connect(dir, id).await?;
    round_trip(stream, request).await
}

/// The same round trip to an address taken from `KELIR_SOCK`, so a process that
/// only inherited the variable (a bash child, the `kelir bg` CLI) can talk to
/// its session without knowing the session dir or id. The session's token comes
/// from `KELIR_TOKEN`, which the session exports to the same children; a
/// request that already carries one is left alone.
pub async fn request_value(address: &str, request: &Request) -> Result<Response, String> {
    let sent = match &request.token {
        Some(_) => request.clone(),
        None => request.clone().with_token(crate::trust::token_from_env()),
    };
    let stream = connect_value(address).await?;
    round_trip(stream, &sent).await
}

async fn round_trip(stream: LocalStream, request: &Request) -> Result<Response, String> {
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read);
    let mut line = serde_json::to_string(request)
        .map_err(|error| format!("cannot serialise the request: {error}"))?;
    line.push('\n');
    let answer = async {
        write
            .write_all(line.as_bytes())
            .await
            .map_err(|error| format!("cannot write to the session socket: {error}"))?;
        write
            .flush()
            .await
            .map_err(|error| format!("cannot flush the session socket: {error}"))?;
        let mut answer = String::new();
        let read = lines
            .read_line(&mut answer)
            .await
            .map_err(|error| format!("cannot read the answer: {error}"))?;
        if read == 0 {
            return Err("the session closed the connection without answering".to_string());
        }
        serde_json::from_str(answer.trim())
            .map_err(|error| format!("the answer is not JSON: {error}"))
    };
    match tokio::time::timeout(IO_TIMEOUT, answer).await {
        Ok(result) => result,
        Err(_) => Err("the session did not answer in time".to_string()),
    }
}

/// Connect to an address as exported in `KELIR_SOCK`: a Unix socket path or a
/// Windows pipe name (`\\.\pipe\kelir-<id>`).
pub async fn connect_value(address: &str) -> Result<LocalStream, String> {
    let address = address.trim();
    #[cfg(unix)]
    let options = {
        let name = Path::new(address)
            .to_fs_name::<GenericFilePath>()
            .map_err(|error| format!("cannot name the session socket: {error}"))?;
        ConnectOptions::new().name(name)
    };
    #[cfg(windows)]
    let options = {
        let pipe = address.trim_start_matches(r"\\.\pipe\").to_string();
        let name = pipe
            .to_ns_name::<GenericNamespaced>()
            .map_err(|error| format!("cannot name the session pipe: {error}"))?;
        ConnectOptions::new().name(name)
    };
    #[cfg(not(any(unix, windows)))]
    let options = {
        let name = Path::new(address)
            .to_fs_name::<GenericFilePath>()
            .map_err(|error| format!("cannot name the session socket: {error}"))?;
        ConnectOptions::new().name(name)
    };
    let connect = options.connect_tokio();
    match tokio::time::timeout(IO_TIMEOUT, connect).await {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("connecting timed out".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn a_request_defaults_its_sender() {
        let request: Request = serde_json::from_str(r#"{"verb":"end"}"#).unwrap();
        assert_eq!(request.from, "ctl");
        assert_eq!(request.args, Value::Null);
    }

    #[test]
    fn ok_and_error_responses_are_one_line_json() {
        let ok = Response::ok("say", 7, "now", json!({"step": 2}));
        let line = ok.line();
        assert!(!line.contains('\n'), "{line}");
        let back: Response = serde_json::from_str(&line).unwrap();
        assert_eq!(back, ok);
        assert_eq!(back.seq, Some(7));
        assert_eq!(back.applies.as_deref(), Some("now"));

        let error = Response::error("unknown verb `x`");
        assert!(!error.ok);
        assert!(!error.line().contains("verb\":"));
    }

    #[test]
    fn the_address_is_a_path_on_unix_and_a_pipe_name_on_windows() {
        let value = value(Path::new("state/sessions/ses_1"), "ses_1");
        if cfg!(windows) {
            assert_eq!(value, r"\\.\pipe\kelir-ses_1");
        } else {
            assert!(value.ends_with("sessions/ses_1/sock"), "{value}");
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kelir-ipc-{name}-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Bind, answer one request, and return the answer.
    async fn round_trip(dir: &Path, id: &str) -> Response {
        let trust = Arc::new(crate::trust::Trust::create(dir.to_path_buf()).unwrap());
        let token = trust.token().to_string();
        let listener = bind(dir, id).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel::<Incoming>();
        serve(listener, trust, tx);
        tokio::spawn(async move {
            let incoming = rx.recv().await.unwrap();
            assert_eq!(incoming.request.verb, "say");
            incoming
                .reply
                .send(Response::ok("say", 3, "queued", json!({})))
                .unwrap();
        });
        request_with_token_file(dir, id, &Request::new("say", json!({"text": "hi"}), "ctl"))
            .await
            .unwrap_or_else(|_| {
                // The token file is the client's route, so a failure here is
                // the route's, not the listener's: prove the token matched.
                assert_eq!(
                    crate::trust::read_token(dir).as_deref(),
                    Some(token.as_str())
                );
                panic!("round trip failed")
            })
    }

    #[tokio::test]
    async fn a_round_trip_carries_the_request_to_the_session() {
        let id = "ses_ipc_test";
        // Shaped like a real session dir, so the address carries the id.
        let dir = temp_dir("round-trip").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let response = round_trip(&dir, id).await;
        assert!(response.ok);
        assert_eq!(response.seq, Some(3));
        assert!(value(&dir, id).contains(id));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    /// The regression behind the first Linux round: `connect` used to unlink
    /// the socket path on its way in, so every client deleted the listener and
    /// got `No such file or directory (os error 2)`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_client_leaves_the_listener_path_alone() {
        let dir = temp_dir("client-unlink");
        let id = "ses_ipc_client";
        let listener = bind(&dir, id).unwrap();
        let sock = dir.join(SOCKET_FILE);
        assert!(sock.exists(), "bind creates the socket file");
        let _ = connect(&dir, id).await;
        assert!(sock.exists(), "a client removed the live socket file");
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A state dir deeper than `sun_path` still gets a working socket: the
    /// session binds a short recorded path, and the client reads the record.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_too_long_session_dir_binds_a_short_recorded_socket() {
        let parent = temp_dir("long");
        let id = "ses_ipc_long";
        let dir = parent.join("d".repeat(MAX_SOCKET_PATH));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!fits(&dir.join(SOCKET_FILE)), "the dir is not long enough");
        if !fits(&fallback_path(id)) {
            // The temp dir itself is too long for a socket; nothing to prove.
            let _ = std::fs::remove_dir_all(&parent);
            return;
        }

        let response = round_trip(&dir, id).await;
        assert!(response.ok);
        let recorded = std::fs::read_to_string(dir.join(SOCKET_POINTER_FILE)).unwrap();
        assert!(fits(Path::new(recorded.trim())), "{recorded}");
        assert_eq!(value(&dir, id), recorded.trim(), "clients read the record");
        let _ = std::fs::remove_file(recorded.trim());
        let _ = std::fs::remove_dir_all(&parent);
    }

    /// N3: only one process can hold a session's log, and releasing the guard
    /// releases the lock. Repeated, because a one-shot pass proves nothing.
    #[cfg(unix)]
    #[test]
    fn only_one_holder_can_lock_a_session() {
        let dir = temp_dir("lock");
        for round in 0..5 {
            let first = lock(&dir).unwrap_or_else(|error| panic!("round {round}: {error}"));
            assert!(lock(&dir).is_err(), "round {round}: a second holder got in");
            assert!(lock(&dir).is_err(), "round {round}: a third holder got in");
            drop(first);
            assert!(
                lock(&dir).is_ok(),
                "round {round}: the lock outlived its guard"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The race the connect-then-bind check cannot win: two lockers start
    /// together. Exactly one wins, twenty times over.
    #[cfg(unix)]
    #[test]
    fn simultaneous_lockers_have_exactly_one_winner() {
        use std::sync::{Arc, Barrier};
        for round in 0..20 {
            let dir = temp_dir("lock-race");
            let start = Arc::new(Barrier::new(2));
            let held = Arc::new(Barrier::new(2));
            let mut winners = 0usize;
            std::thread::scope(|scope| {
                let mut handles = Vec::new();
                for _ in 0..2 {
                    let dir = dir.clone();
                    let start = start.clone();
                    let held = held.clone();
                    handles.push(scope.spawn(move || {
                        start.wait();
                        let guard = lock(&dir).ok();
                        // The loser's attempt lands while the winner still
                        // holds the lock: both reach `held` before either drops.
                        held.wait();
                        let won = guard.is_some();
                        drop(guard);
                        won
                    }));
                }
                for handle in handles {
                    if handle.join().unwrap() {
                        winners += 1;
                    }
                }
            });
            assert_eq!(winners, 1, "round {round}: {winners} winners");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[tokio::test]
    async fn an_unknown_socket_is_a_clean_error() {
        let dir = temp_dir("dead");
        let error = connect(&dir, "ses_dead").await.unwrap_err();
        assert!(!error.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `KELIR_SOCK` alone must be enough to reach the session: `kelir bg` has
    /// nothing else to go on inside a bash call. The token comes with it
    /// (`KELIR_TOKEN`), which is what makes the report acceptable (STEER-7).
    #[tokio::test]
    async fn the_exported_socket_value_reaches_the_session() {
        let id = "ses_ipc_value";
        let dir = temp_dir("value").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let trust = Arc::new(crate::trust::Trust::create(dir.clone()).unwrap());
        let token = trust.token().to_string();
        let listener = bind(&dir, id).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel::<Incoming>();
        serve(listener, trust, tx);
        tokio::spawn(async move {
            if let Some(incoming) = rx.recv().await {
                assert_eq!(incoming.request.verb, "bg");
                assert_eq!(incoming.request.from, "job");
                let _ = incoming.reply.send(Response::ok("bg", 4, "now", json!({})));
            }
        });
        let answer = request_value(
            &value(&dir, id),
            &Request::new("bg", json!({}), "job").with_token(Some(token.clone())),
        )
        .await
        .unwrap();
        assert!(answer.ok);
        assert_eq!(answer.seq, Some(4));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    /// A peer is read for at most `MAX_LINE` bytes and `IDLE_TIMEOUT`: an
    /// unterminated megabyte is refused at the cap instead of being held, and a
    /// connection that says nothing is closed rather than kept (STEER-7 review
    /// N3).
    #[tokio::test]
    async fn a_request_line_is_bounded_in_length_and_in_time() {
        use tokio::io::AsyncWriteExt as _;
        // A whole line is a line.
        let (client, server) = tokio::io::duplex(MAX_LINE * 2);
        let mut reader = BufReader::new(server);
        let mut line = String::new();
        let mut client = client;
        client.write_all(b"{\"verb\":\"say\"}\n").await.unwrap();
        assert!(matches!(
            read_bounded(&mut reader, &mut line, Duration::from_secs(1)).await,
            Read::Line
        ));
        assert_eq!(line.trim(), "{\"verb\":\"say\"}");

        // Longer than the cap, never ended: refused, and the buffer stops at the
        // cap (the rest is never read).
        let (mut client, server) = tokio::io::duplex(MAX_LINE * 2);
        let writer = tokio::spawn(async move {
            let chunk = vec![b'x'; 4096];
            for _ in 0..(MAX_LINE / 4096 + 2) {
                if client.write_all(&chunk).await.is_err() {
                    return;
                }
            }
        });
        let mut reader = BufReader::new(server);
        let mut line = String::new();
        match read_bounded(&mut reader, &mut line, Duration::from_secs(5)).await {
            Read::Refused(message) => assert!(message.contains("longer than"), "{message}"),
            _ => panic!("an unterminated line past the cap was accepted"),
        }
        assert!(line.len() <= MAX_LINE, "the buffer grew to {}", line.len());
        let _ = writer.await;

        // A peer that connects and says nothing is not a client.
        let (_client, server) = tokio::io::duplex(64);
        let mut reader = BufReader::new(server);
        let mut line = String::new();
        match read_bounded(&mut reader, &mut line, Duration::from_millis(50)).await {
            Read::Refused(message) => assert!(message.contains("no request line"), "{message}"),
            _ => panic!("a silent peer was held open"),
        }

        // A peer that leaves before its line is simply gone.
        let (client, server) = tokio::io::duplex(64);
        drop(client);
        let mut reader = BufReader::new(server);
        let mut line = String::new();
        assert!(matches!(
            read_bounded(&mut reader, &mut line, Duration::from_secs(1)).await,
            Read::Closed
        ));
    }

    /// The socket half of STEER-7: a request with no token, or another
    /// session's, is answered and refused at the listener — the session never
    /// sees it, so it cannot act on it or log it.
    #[tokio::test]
    async fn an_unauthenticated_request_never_reaches_the_session() {
        let id = "ses_ipc_token";
        let dir = temp_dir("token").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let trust = Arc::new(crate::trust::Trust::create(dir.clone()).unwrap());
        let listener = bind(&dir, id).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel::<Incoming>();
        serve(listener, trust, tx);
        let seen = tokio::spawn(async move {
            // Nothing should ever arrive; the test ends the task.
            tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .ok()
                .flatten()
                .map(|incoming| incoming.request.verb)
        });

        for request in [
            Request::new("say", json!({"text": "hi"}), "ctl"),
            Request::new("say", json!({"text": "hi"}), "ctl")
                .with_token(Some("a-token-from-another-session".into())),
            Request::new("bg", json!({}), "job"),
        ] {
            let answer = super::request(&dir, id, &request).await.unwrap();
            assert!(!answer.ok, "{answer:?}");
            let error = answer.error.unwrap_or_default();
            assert!(error.contains("refused"), "{error}");
            assert!(!error.contains("a-token-from-another-session"));
        }
        assert_eq!(seen.await.unwrap(), None, "the session saw the request");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
}
