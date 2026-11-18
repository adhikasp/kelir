//! The session socket: newline-delimited JSON over a Unix socket (a named pipe
//! on Windows), served by the session process.
//!
//! Request: `{"verb": "say", "args": {"text": "hi"}, "from": "ctl"}`.
//! Answer: one line, `{"ok": true, …}` or `{"ok": false, "error": "…"}`.
//!
//! The path is `<session dir>/sock` on Unix and `\\.\pipe\kelir-<session id>`
//! on Windows; both open with the same code, so `kelir ctl` never needs to know
//! which platform the session runs on. The address is also exported to every
//! bash child as `KELIR_SOCK`.

use std::path::Path;
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
/// Named-pipe prefix on Windows.
const PIPE_PREFIX: &str = "kelir-";
/// How long a client waits for a connection or an answer.
pub const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// One request line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub verb: String,
    #[serde(default)]
    pub args: Value,
    /// Who sent it: `ctl`, `tui`, `gate`…
    #[serde(default = "default_from")]
    pub from: String,
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
        }
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
}

/// The address of a session's socket, for `KELIR_SOCK`, error messages and the
/// client.
#[cfg(unix)]
pub fn value(dir: &Path, _id: &str) -> String {
    crate::util::path_string(&dir.join(SOCKET_FILE))
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
        let path = dir.join(SOCKET_FILE);
        // A stale file from a session that died without cleaning up would make
        // bind fail; it cannot be a live listener, since the session dir is new.
        let _ = std::fs::remove_file(&path);
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

/// Bind the listener. Done before `session_started` is recorded, so a client
/// that sees the event can connect.
pub fn bind(dir: &Path, id: &str) -> Result<LocalListener, String> {
    let address = name(dir, id)?;
    let listener = ListenerOptions::new()
        .name(address)
        .create_tokio()
        .map_err(|error| {
            format!(
                "cannot listen on the session socket {}: {error}",
                value(dir, id)
            )
        })?;
    // `say` is a prompt-injection channel into a fully autonomous agent, so the
    // socket is the user's alone: 0600 on Unix (a umask like 002 would
    // otherwise let the group steer it). The Windows pipe DACL already denies
    // other users write (M1 review N4).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(
            dir.join(SOCKET_FILE),
            std::fs::Permissions::from_mode(0o600),
        );
    }
    Ok(listener)
}

/// Accept connections forever. Every connection gets its own task; answers go
/// back on each request's own channel, so two clients never block each other.
pub fn serve(listener: LocalListener, tx: mpsc::UnboundedSender<Incoming>) {
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok(stream) => {
                    let tx = tx.clone();
                    tokio::spawn(async move { connection(stream, tx).await });
                }
                // The listener only fails for good reasons (the pipe name was
                // taken, the runtime is going away); retry so one bad accept
                // cannot stop the session from being reachable.
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    });
}

async fn connection(stream: LocalStream, tx: mpsc::UnboundedSender<Incoming>) {
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read);
    let mut line = String::new();
    loop {
        line.clear();
        match lines.read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(trimmed) {
            Ok(request) => {
                let (reply, answer) = oneshot::channel();
                if tx.send(Incoming { request, reply }).is_err() {
                    let _ = write
                        .write_all(Response::error("the session is ending").line().as_bytes())
                        .await;
                    let _ = write.write_all(b"\n").await;
                    let _ = write.flush().await;
                    return;
                }
                match answer.await {
                    Ok(response) => response,
                    Err(_) => Response::error("the session did not answer"),
                }
            }
            Err(error) => Response::error(format!("bad request line: {error}")),
        };
        let mut out = response.line();
        out.push('\n');
        if write.write_all(out.as_bytes()).await.is_err() || write.flush().await.is_err() {
            return;
        }
    }
}

/// Connect to a session's socket.
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
    let stream = connect(dir, id).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    #[tokio::test]
    async fn a_round_trip_carries_the_request_to_the_session() {
        let dir = std::env::temp_dir().join(format!("kelir-ipc-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let id = "ses_ipc_test";
        let listener = bind(&dir, id).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel::<Incoming>();
        serve(listener, tx);

        let addr = value(&dir, id);
        let task = tokio::spawn(async move {
            let incoming = rx.recv().await.unwrap();
            assert_eq!(incoming.request.verb, "say");
            incoming
                .reply
                .send(Response::ok("say", 3, "queued", json!({})))
                .unwrap();
        });

        let response = request(&dir, id, &Request::new("say", json!({"text": "hi"}), "ctl"))
            .await
            .unwrap();
        assert!(response.ok);
        assert_eq!(response.seq, Some(3));
        task.await.unwrap();
        assert!(addr.contains(id));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_unknown_socket_is_a_clean_error() {
        let dir = std::env::temp_dir().join(format!("kelir-ipc-dead-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let error = connect(&dir, "ses_dead").await.unwrap_err();
        assert!(!error.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
