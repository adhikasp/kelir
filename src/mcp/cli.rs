//! `kelir mcp …`: the client side.
//!
//! Inside a session (`KELIR_SOCK` set) every request goes to the session's
//! broker, which keeps servers alive and logs the call. Outside one, or when
//! the socket cannot be reached (a background job that outlived its session),
//! the CLI starts the server itself for this one command.

use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use super::args::{self, CallArgs, Command, Detail};
use super::broker;
use super::config::{self, ServerConfig};
use super::render::{EXIT_TIMEOUT, EXIT_UNAVAILABLE, EXIT_USAGE};
use super::rpc::Client;
use super::{CallOutput, run_call};
use crate::ipc::{self, Request};

/// How long the CLI waits on the broker: longer than any one call can take
/// (server start plus the call's own timeout plus slack).
const BROKER_WAIT: Duration = Duration::from_secs(30 * 60);

/// A failure with the exit code it maps to.
#[derive(Debug)]
struct Fail {
    code: i32,
    message: String,
}

impl Fail {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

enum Backend {
    Session(String),
    OneShot(OneShot),
}

struct OneShot {
    servers: BTreeMap<String, ServerConfig>,
    config_error: Option<String>,
    cwd: PathBuf,
    clients: HashMap<String, (Arc<Client>, Vec<Value>)>,
    starts: u64,
}

impl OneShot {
    fn new() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let (servers, config_error) = match config::load(&cwd) {
            Ok(servers) => (servers, None),
            Err(error) => (BTreeMap::new(), Some(error)),
        };
        Self {
            servers,
            config_error,
            cwd,
            clients: HashMap::new(),
            starts: 0,
        }
    }

    fn unknown(&self, name: &str) -> Fail {
        let known: Vec<&str> = self.servers.keys().map(String::as_str).collect();
        let mut message = format!("kelir mcp: unknown server `{name}`");
        if let Some(error) = &self.config_error {
            message.push_str(&format!(" ({error})"));
        } else if known.is_empty() {
            message.push_str(" (no servers are configured: see `kelir docs config`)");
        } else {
            message.push_str(&format!(" (configured: {})", known.join(", ")));
        }
        Fail::new(EXIT_USAGE, message)
    }

    async fn ensure(&mut self, name: &str) -> Result<(Arc<Client>, Vec<Value>), Fail> {
        if let Some(pair) = self.clients.get(name) {
            return Ok(pair.clone());
        }
        let Some(config) = self.servers.get(name).cloned() else {
            return Err(self.unknown(name));
        };
        self.starts += 1;
        let label = format!(
            "kelir-mcp-oneshot-{}-{name}-{}",
            std::process::id(),
            self.starts
        );
        let client = Client::start(&config, &self.cwd, &label, None, None)
            .await
            .map_err(|error| {
                Fail::new(EXIT_UNAVAILABLE, format!("kelir mcp: {}", error.message()))
            })?;
        let client = Arc::new(client);
        let tools = match client.list_tools(config.timeout).await {
            Ok(tools) => tools,
            Err(super::rpc::CallError::Rpc { .. }) => Vec::new(),
            Err(error) => {
                client.kill();
                return Err(Fail::new(
                    EXIT_UNAVAILABLE,
                    format!("kelir mcp: {}", error.message()),
                ));
            }
        };
        self.clients
            .insert(name.to_string(), (client.clone(), tools.clone()));
        Ok((client, tools))
    }
}

impl Backend {
    fn choose() -> Backend {
        match crate::envs::var("KELIR_SOCK") {
            Some(address) if !address.trim().is_empty() => Backend::Session(address),
            _ => Backend::OneShot(OneShot::new()),
        }
    }

    /// One request to the broker. `Ok(None)` means the session cannot be
    /// reached, and the caller falls back to one-shot. Only a failed *connect*
    /// qualifies for a `retryable == false` request (a tool call): once the
    /// request may have been delivered, the outcome is unknown and the call is
    /// never run again (exit 4).
    async fn ask(address: &str, args: Value, retryable: bool) -> Result<Option<Value>, Fail> {
        let request = Request::new(broker::VERB, args, "mcp");
        let response = match ipc::request_value_within(address, &request, BROKER_WAIT).await {
            Ok(response) => response,
            Err(ipc::WithinError::NotConnected(_)) => return Ok(None),
            Err(ipc::WithinError::AfterConnect(_)) if retryable => return Ok(None),
            Err(ipc::WithinError::AfterConnect(error)) => {
                return Err(Fail::new(
                    EXIT_TIMEOUT,
                    format!(
                        "kelir mcp: lost the session while the call ran ({error}); the outcome                          is unknown and the call is not retried"
                    ),
                ));
            }
        };
        if !response.ok && !retryable && response.error.as_deref() == Some(ipc::DROPPED_REPLY) {
            return Err(Fail::new(
                EXIT_TIMEOUT,
                "kelir mcp: the session ended while the call ran; the outcome is unknown and                  the call is not retried",
            ));
        }
        if !response.ok {
            return Err(Fail::new(
                EXIT_UNAVAILABLE,
                format!(
                    "kelir mcp: the session refused the request: {}",
                    response.error.unwrap_or_default()
                ),
            ));
        }
        let state = response.state.unwrap_or(Value::Null);
        if let Some(error) = state.get("error") {
            return Err(Fail::new(
                error
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .unwrap_or(i64::from(EXIT_UNAVAILABLE)) as i32,
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("kelir mcp: failed")
                    .to_string(),
            ));
        }
        Ok(Some(state))
    }

    /// Switch to one-shot after the socket failed.
    fn degrade(&mut self) {
        eprintln!("kelir mcp: the session socket is not reachable; running this call one-shot");
        *self = Backend::OneShot(OneShot::new());
    }

    async fn servers(&mut self) -> Result<Value, Fail> {
        if let Backend::Session(address) = self {
            match Self::ask(address, json!({"op": "servers"}), true).await? {
                Some(state) => return Ok(state),
                None => self.degrade(),
            }
        }
        let Backend::OneShot(shot) = self else {
            unreachable!("degraded to one-shot")
        };
        let list: Vec<Value> = shot
            .servers
            .values()
            .map(|server| {
                json!({"name": server.name, "state": "idle", "tools": null,
                       "channel": server.channel, "description": server.description})
            })
            .collect();
        Ok(json!({"servers": list, "config_error": shot.config_error}))
    }

    async fn tools(&mut self, server: &str) -> Result<Vec<Value>, Fail> {
        if let Backend::Session(address) = self {
            match Self::ask(address, json!({"op": "tools", "server": server}), true).await? {
                Some(state) => {
                    return Ok(state
                        .get("tools")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default());
                }
                None => self.degrade(),
            }
        }
        let Backend::OneShot(shot) = self else {
            unreachable!("degraded to one-shot")
        };
        shot.ensure(server).await.map(|(_, tools)| tools)
    }

    async fn call(
        &mut self,
        server: &str,
        tool: &str,
        arguments: Value,
        envelope: bool,
    ) -> Result<CallOutput, Fail> {
        if let Backend::Session(address) = self {
            let parent = crate::envs::var("KELIR_CALL_ID").filter(|id| !id.trim().is_empty());
            let request = json!({
                "op": "call", "server": server, "tool": tool, "args": arguments,
                "envelope": envelope, "parent_call_id": parent,
            });
            match Self::ask(address, request, false).await? {
                Some(state) => {
                    return serde_json::from_value(state).map_err(|error| {
                        Fail::new(
                            EXIT_UNAVAILABLE,
                            format!("kelir mcp: the session's answer is not a result: {error}"),
                        )
                    });
                }
                None => self.degrade(),
            }
        }
        let Backend::OneShot(shot) = self else {
            unreachable!("degraded to one-shot")
        };
        let (client, _) = shot.ensure(server).await?;
        let config = shot
            .servers
            .get(server)
            .cloned()
            .ok_or_else(|| shot.unknown(server))?;
        let out_dir = crate::paths::state_dir()
            .join("mcp")
            .join(ulid::Ulid::generate().to_string());
        Ok(run_call(&client, &config, tool, arguments, envelope, &out_dir, false).await)
    }

    /// The server children die with the command.
    fn finish(&mut self) {
        if let Backend::OneShot(shot) = self {
            for (client, _) in shot.clients.values() {
                client.kill();
            }
            shot.clients.clear();
        }
    }
}

fn emit(text: &str, to_stderr: bool) {
    if text.is_empty() {
        return;
    }
    let newline = if text.ends_with('\n') { "" } else { "\n" };
    if to_stderr {
        let _ = write!(std::io::stderr(), "{text}{newline}");
    } else {
        let mut out = std::io::stdout().lock();
        let _ = write!(out, "{text}{newline}");
        let _ = out.flush();
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim()
}

/// `kelir mcp …`. Returns the process exit code.
pub async fn run(words: Vec<String>) -> u8 {
    let command = match args::parse(&words) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("kelir mcp: {error}\n{}", args::USAGE);
            return EXIT_USAGE as u8;
        }
    };
    let mut backend = Backend::choose();
    let result = execute(&mut backend, command).await;
    backend.finish();
    match result {
        Ok(code) => code as u8,
        Err(fail) => {
            emit(&fail.message, true);
            fail.code as u8
        }
    }
}

async fn execute(backend: &mut Backend, command: Command) -> Result<i32, Fail> {
    match command {
        Command::Servers { json } => {
            let state = backend.servers().await?;
            let list = state
                .get("servers")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if json {
                emit(&Value::Array(list).to_string(), false);
                return Ok(0);
            }
            if let Some(error) = state.get("config_error").and_then(Value::as_str) {
                emit(&format!("kelir mcp: {error}"), true);
            }
            if list.is_empty() {
                emit("no MCP servers configured (see `kelir docs config`)", false);
                return Ok(0);
            }
            let text = |entry: &Value, key: &str| {
                entry
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            let width = list
                .iter()
                .map(|entry| text(entry, "name").len())
                .max()
                .unwrap_or(0);
            let state_width = list
                .iter()
                .map(|entry| text(entry, "state").len())
                .max()
                .unwrap_or(0);
            for entry in &list {
                let tools = entry
                    .get("tools")
                    .and_then(Value::as_u64)
                    .map(|n| format!("{n} tools"))
                    .unwrap_or_else(|| "- tools".into());
                let channel = if entry.get("channel") == Some(&Value::Bool(true)) {
                    " [channel]"
                } else {
                    ""
                };
                let line = format!(
                    "{:width$}  {:state_width$}  {tools}{channel}  {}",
                    text(entry, "name"),
                    text(entry, "state"),
                    first_line(&text(entry, "description")),
                );
                emit(line.trim_end(), false);
            }
            Ok(0)
        }
        Command::Tools { server, detail } => {
            let tools = backend.tools(&server).await?;
            match detail {
                Detail::Full => {
                    emit(
                        &serde_json::to_string_pretty(&tools).unwrap_or_default(),
                        false,
                    );
                }
                Detail::Name | Detail::Desc => {
                    for tool in &tools {
                        let name = tool.get("name").and_then(Value::as_str).unwrap_or("?");
                        let description = tool
                            .get("description")
                            .and_then(Value::as_str)
                            .map(first_line)
                            .unwrap_or("");
                        if detail == Detail::Name || description.is_empty() {
                            emit(name, false);
                        } else {
                            emit(&format!("{name}  {description}"), false);
                        }
                    }
                }
            }
            Ok(0)
        }
        Command::Call(call) => call_tool(backend, call).await,
    }
}

async fn call_tool(backend: &mut Backend, call: CallArgs) -> Result<i32, Fail> {
    let tools = backend.tools(&call.server).await?;
    let Some(tool) = tools
        .iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(call.tool.as_str()))
    else {
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect();
        return Err(Fail::new(
            EXIT_USAGE,
            format!(
                "kelir mcp: `{}` has no tool `{}` (tools: {})",
                call.server,
                call.tool,
                names.join(", ")
            ),
        ));
    };
    let schema = tool.get("inputSchema").cloned().unwrap_or(Value::Null);
    if call.schema {
        let shown = if schema.is_null() { json!({}) } else { schema };
        emit(
            &serde_json::to_string_pretty(&shown).unwrap_or_default(),
            false,
        );
        return Ok(0);
    }
    let json_text = match call.json.as_deref() {
        Some("-") => {
            let mut text = String::new();
            use tokio::io::AsyncReadExt as _;
            tokio::io::stdin()
                .read_to_string(&mut text)
                .await
                .map_err(|error| {
                    Fail::new(EXIT_USAGE, format!("kelir mcp: cannot read stdin: {error}"))
                })?;
            Some(text)
        }
        other => other.map(str::to_string),
    };
    let arguments = args::build(&call.pairs, json_text.as_deref(), &schema)
        .map_err(|error| Fail::new(EXIT_USAGE, format!("kelir mcp: {error}")))?;
    args::validate(&schema, &arguments).map_err(|problems| {
        Fail::new(
            EXIT_USAGE,
            format!(
                "kelir mcp: invalid arguments for {}.{}:\n{problems}\n(`kelir mcp {} {} --schema` \
                 prints the schema)",
                call.server, call.tool, call.server, call.tool
            ),
        )
    })?;
    let output = backend
        .call(&call.server, &call.tool, arguments, call.envelope)
        .await?;
    emit(&output.stdout, false);
    emit(&output.stderr, true);
    Ok(output.exit_code)
}
