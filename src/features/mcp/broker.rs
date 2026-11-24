//! The session side of `kelir mcp`: the broker.
//! One [`Broker`] lives in the session process.
//! It owns a connection per server, started lazily by the first call that needs it (channel
//! servers are started when the session starts), keeps it for the session and restarts it the next
//! time a call finds it dead.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::config::ServerConfig;
use super::render::{EXIT_UNAVAILABLE, EXIT_USAGE};
use super::rpc::{CallError, Client, Notification};
use super::{CallOutput, run_call};
use crate::ipc::{Incoming, Request, Response};

pub const VERB: &str = "mcp";
pub const VERB_SETTLE: &str = "mcp-settle";
pub const VERB_NOTIFY: &str = "mcp-notify";
/// The `from` of an internal request.
pub const FROM_INTERNAL: &str = "mcp-internal";

/// Is this verb the broker's? (The session intercepts these before the normal
/// verb path.)
pub fn is_mcp_verb(name: &str) -> bool {
    matches!(name, VERB | VERB_SETTLE | VERB_NOTIFY)
}

#[derive(Default)]
struct Slot {
    client: Option<Arc<Client>>,
    tools: Option<Vec<Value>>,
    failure: Option<String>,
}

struct Entry {
    config: ServerConfig,
    slot: tokio::sync::Mutex<Slot>,
}

struct Inner {
    servers: BTreeMap<String, Arc<Entry>>,
    config_error: Option<String>,
    cwd: PathBuf,
    session_dir: PathBuf,
    session_id: String,
    key: String,
    notify: Mutex<Option<mpsc::UnboundedSender<Notification>>>,
    /// Channel mode: when set, inbound notifications go straight to the
    /// headless driver instead of a session loop (which does not exist).
    channel_sink: Mutex<Option<mpsc::UnboundedSender<Notification>>>,
    starts: AtomicU64,
}

#[derive(Clone)]
pub struct Broker(Arc<Inner>);

impl Broker {
    /// Read `[mcp.servers.*]` from the explicit file (`--config`) or both config
    /// layers for a session whose working directory is `cwd`. A bad file is
    /// remembered, not fatal: the session still runs, and `kelir mcp` says what is wrong.
    pub fn new(
        cwd: PathBuf,
        session_dir: PathBuf,
        session_id: String,
        explicit: Option<&Path>,
    ) -> Broker {
        let (servers, config_error) = match crate::config::load_for_cwd(explicit, &cwd) {
            Ok(cfg) => (cfg.mcp, None),
            Err(error) => (BTreeMap::new(), Some(error.to_string())),
        };
        Broker::with_servers(servers, config_error, cwd, session_dir, session_id)
    }

    pub fn with_servers(
        servers: BTreeMap<String, ServerConfig>,
        config_error: Option<String>,
        cwd: PathBuf,
        session_dir: PathBuf,
        session_id: String,
    ) -> Broker {
        let servers = servers
            .into_iter()
            .map(|(name, config)| {
                (
                    name,
                    Arc::new(Entry {
                        config,
                        slot: tokio::sync::Mutex::new(Slot::default()),
                    }),
                )
            })
            .collect();
        Broker(Arc::new(Inner {
            servers,
            config_error,
            cwd,
            session_dir,
            session_id,
            key: ulid::Ulid::generate().to_string(),
            notify: Mutex::new(None),
            channel_sink: Mutex::new(None),
            starts: AtomicU64::new(0),
        }))
    }

    /// The nonce internal verbs must carry.
    pub fn key(&self) -> &str {
        &self.0.key
    }

    pub fn config_error(&self) -> Option<&str> {
        self.0.config_error.as_deref()
    }

    pub fn has_server(&self, name: &str) -> bool {
        self.0.servers.contains_key(name)
    }

    pub fn server_names(&self) -> Vec<String> {
        self.0.servers.keys().cloned().collect()
    }

    /// `(name, description)` of every configured server, for the hint and
    /// `observe`.
    pub fn descriptions(&self) -> Vec<(String, String)> {
        self.0
            .servers
            .values()
            .map(|entry| (entry.config.name.clone(), entry.config.description.clone()))
            .collect()
    }

    /// Connect the broker to the session loop: inbound notifications travel
    /// as `mcp-notify` verbs, and channel servers are started now.
    pub fn attach(&self, inbox: mpsc::UnboundedSender<Incoming>) {
        let (tx, mut rx) = mpsc::unbounded_channel::<Notification>();
        if let Ok(mut slot) = self.0.notify.lock() {
            *slot = Some(tx);
        }
        let forward = inbox.clone();
        let key = self.0.key.clone();
        tokio::spawn(async move {
            while let Some(note) = rx.recv().await {
                let args = json!({
                    "key": key, "server": note.server,
                    "method": note.method, "params": note.params,
                });
                let (reply, _dropped) = oneshot::channel();
                let sent = forward.send(Incoming {
                    request: Request::new(VERB_NOTIFY, args, FROM_INTERNAL),
                    reply,
                    action: None,
                });
                if sent.is_err() {
                    return;
                }
            }
        });
        for entry in self.0.servers.values() {
            if !entry.config.channel {
                continue;
            }
            let broker = self.clone();
            let name = entry.config.name.clone();
            let inbox = inbox.clone();
            tokio::spawn(async move {
                if let Err(error) = broker.ensure(&name).await {
                    // A channel that cannot connect is worth a line in the log:
                    // nothing else would ever say that messages are not coming.
                    let args = json!({
                        "key": broker.key(), "server": name,
                        "method": "kelir/unavailable",
                        "params": {"message": error.message()},
                    });
                    let (reply, _dropped) = oneshot::channel();
                    let _ = inbox.send(Incoming {
                        request: Request::new(VERB_NOTIFY, args, FROM_INTERNAL),
                        reply,
                        action: None,
                    });
                }
            });
        }
    }

    /// Give the broker a direct sink for raw MCP notifications (`kelir channel`). A session's
    /// `attach` is unaffected; when both exist the direct sink wins for inbound messages.
    pub fn set_channel_sink(&self, sink: mpsc::UnboundedSender<Notification>) {
        if let Ok(mut slot) = self.0.channel_sink.lock() {
            *slot = Some(sink);
        }
    }

    /// The state of every server, for `kelir mcp ls`. Never starts anything.
    pub fn servers(&self) -> Value {
        let list: Vec<Value> = self
            .0
            .servers
            .values()
            .map(|entry| {
                let (state, tools) = match entry.slot.try_lock() {
                    Err(_) => ("starting".to_string(), None),
                    Ok(slot) => {
                        let tools = slot.tools.as_ref().map(Vec::len);
                        match (&slot.client, &slot.failure) {
                            (Some(client), _) if client.is_alive() => (
                                if entry.config.channel {
                                    "connected"
                                } else {
                                    "running"
                                }
                                .to_string(),
                                tools,
                            ),
                            (_, Some(failure)) => (format!("failed: {failure}"), tools),
                            (Some(_), None) => ("exited".to_string(), tools),
                            (None, None) => ("idle".to_string(), tools),
                        }
                    }
                };
                json!({
                    "name": entry.config.name,
                    "state": state,
                    "tools": tools,
                    "channel": entry.config.channel,
                    "reply_tool": entry.config.reply_tool,
                    "description": entry.config.description,
                })
            })
            .collect();
        json!({"servers": list, "config_error": self.0.config_error})
    }

    /// A live connection to `name` and its tools, starting the server if it is
    /// not running.
    pub async fn ensure(&self, name: &str) -> Result<(Arc<Client>, Vec<Value>), CallError> {
        let Some(entry) = self.0.servers.get(name) else {
            return Err(CallError::Unavailable(format!("unknown server `{name}`")));
        };
        let mut slot = entry.slot.lock().await;
        let client = slot.client.clone();
        // Hold the guard across the awaits below, then drop it before returning
        // so the returned `Arc<Client>` is not tied to the lock's lifetime.
        if let Some(client) = client
            && client.is_alive()
        {
            // The server said its tool list changed: read it again.
            if client.take_tools_changed()
                && let Ok(tools) = client.list_tools(entry.config.timeout).await
            {
                slot.tools = Some(tools);
            }
            let tools = slot.tools.clone().unwrap_or_default();
            drop(slot);
            return Ok((client, tools));
        }
        slot.client = None;
        slot.tools = None;
        let n = self.0.starts.fetch_add(1, Ordering::SeqCst);
        let label = format!("kelir-mcp-{}-{name}-{n}", self.0.session_id);
        let notify = self
            .0
            .channel_sink
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .or_else(|| self.0.notify.lock().ok().and_then(|slot| slot.clone()));
        // A channel server learns that kelir is ready to receive its messages
        // from the capabilities of `initialize`. Never a permission relay.
        let capabilities = if entry.config.channel {
            Some(crate::features::channel::capabilities(&entry.config))
        } else {
            None
        };
        let started = Client::start(&entry.config, &self.0.cwd, &label, notify, capabilities).await;
        let client = match started {
            Ok(client) => Arc::new(client),
            Err(error) => {
                slot.failure = Some(error.message());
                return Err(error);
            }
        };
        let tools = match client.list_tools(entry.config.timeout).await {
            Ok(tools) => tools,
            // A server without tools answers -32601 here; it is still a server.
            Err(CallError::Rpc { .. }) => Vec::new(),
            Err(error) => {
                client.kill();
                slot.failure = Some(error.message());
                return Err(error);
            }
        };
        slot.failure = None;
        slot.client = Some(client.clone());
        slot.tools = Some(tools.clone());
        Ok((client, tools))
    }

    pub async fn tools(&self, name: &str) -> Result<Vec<Value>, CallError> {
        self.ensure(name).await.map(|(_, tools)| tools)
    }

    /// Run one call (already registered in the log by the session).
    pub async fn call(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
        envelope: bool,
        mcp_id: &str,
    ) -> CallOutput {
        let (client, _tools) = match self.ensure(server).await {
            Ok(pair) => pair,
            Err(error) => {
                return CallOutput::failed(
                    EXIT_UNAVAILABLE,
                    "unavailable",
                    format!("kelir mcp: {}", error.message()),
                );
            }
        };
        let Some(entry) = self.0.servers.get(server) else {
            return CallOutput::failed(EXIT_USAGE, "error", format!("unknown server `{server}`"));
        };
        let out_dir = crate::paths::call_dir(&self.0.session_dir, mcp_id);
        run_call(
            &client,
            &entry.config,
            tool,
            arguments,
            envelope,
            &out_dir,
            true,
        )
        .await
    }

    /// The non-logged operations: `servers` and `tools`.
    pub async fn read_op(&self, args: &Value) -> Response {
        let op = args.get("op").and_then(Value::as_str).unwrap_or("");
        match op {
            "servers" => Response::ok(VERB, 0, "now", self.servers()),
            "tools" => {
                let server = args.get("server").and_then(Value::as_str).unwrap_or("");
                if !self.has_server(server) {
                    return Response::ok(VERB, 0, "now", self.unknown_server(server));
                }
                match self.tools(server).await {
                    Ok(tools) => Response::ok(VERB, 0, "now", json!({"tools": tools})),
                    Err(error) => Response::ok(
                        VERB,
                        0,
                        "now",
                        failure(EXIT_UNAVAILABLE, &format!("kelir mcp: {}", error.message())),
                    ),
                }
            }
            other => Response::error(format!("mcp: unknown operation `{other}`")),
        }
    }

    /// The answer for a server that is not configured (usage error).
    pub fn unknown_server(&self, name: &str) -> Value {
        let known = self.server_names();
        let mut message = format!("kelir mcp: unknown server `{name}`");
        if let Some(error) = &self.0.config_error {
            message.push_str(&format!(" ({error})"));
        } else if known.is_empty() {
            message.push_str(" (no servers are configured: see `kelir docs config`)");
        } else {
            message.push_str(&format!(" (configured: {})", known.join(", ")));
        }
        failure(EXIT_USAGE, &message)
    }

    /// Kill every server now (the session is ending).
    pub async fn shutdown(&self) {
        for entry in self.0.servers.values() {
            if let Ok(slot) = entry.slot.try_lock()
                && let Some(client) = &slot.client
            {
                client.kill();
            }
        }
    }
}

/// `{"error": {"exit_code": …, "message": …}}`: the answer to a request that
/// failed in a way that has an exit code.
pub fn failure(exit_code: i32, message: &str) -> Value {
    json!({"error": {"exit_code": exit_code, "message": message}})
}
