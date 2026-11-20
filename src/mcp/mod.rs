//! MCP as a CLI (M6): `kelir mcp …`.
//!
//! The model never gets MCP tool schemas in its request. It runs
//! `kelir mcp <server> <tool> k=v` in a bash call; inside a session that call
//! reaches the **broker** in the session process over `KELIR_SOCK`, which keeps
//! each server alive for the session and writes `mcp_started`/`mcp_settled`.
//! Outside a session the CLI starts the server for that one call.
//!
//! * [`config`] — `mcp.toml`, two layers
//! * [`rpc`] — the JSON-RPC subset over a child's stdio (hand-rolled, see
//!   docs/DEVIATIONS.md)
//! * [`args`] — the command line, `k=v` typing, schema validation
//! * [`render`] — a result as stdout/stderr/files, and the exit codes
//! * [`broker`] — the session side
//! * [`cli`] — the client side

pub mod args;
pub mod broker;
pub mod cli;
pub mod config;
pub mod render;
pub mod rpc;

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use self::config::ServerConfig;
use self::render::{EXIT_TIMEOUT, EXIT_TOOL_ERROR, EXIT_UNAVAILABLE};
use self::rpc::{CallError, Client};

/// A finished call, as the CLI prints it (over the socket or in-process).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallOutput {
    pub exit_code: i32,
    /// `ok`, `error`, `timeout`, `unavailable` or `disabled`.
    pub outcome: String,
    pub stdout: String,
    pub stderr: String,
    /// Size of the text the call produced.
    pub bytes: u64,
    /// Where the full text is on disk (empty for a one-shot call).
    pub out_path: String,
}

impl CallOutput {
    /// A call that never reached a server (or was refused before it).
    pub fn failed(exit_code: i32, outcome: &str, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            exit_code,
            outcome: outcome.into(),
            bytes: message.len() as u64,
            stdout: String::new(),
            stderr: message,
            out_path: String::new(),
        }
    }
}

/// Make one `tools/call` and render it. `out_dir` receives decoded blobs and,
/// when `write_out`, the call's text as `out`.
pub async fn run_call(
    client: &Client,
    config: &ServerConfig,
    tool: &str,
    arguments: Value,
    envelope: bool,
    out_dir: &Path,
    write_out: bool,
) -> CallOutput {
    let result = client.call_tool(tool, arguments, config.timeout).await;
    let mut output = match result {
        Ok(result) => {
            let rendered = render::render(&result, out_dir, envelope);
            CallOutput {
                exit_code: rendered.exit_code(),
                outcome: if rendered.is_error { "error" } else { "ok" }.into(),
                bytes: (rendered.stdout.len() + rendered.stderr.len()) as u64,
                stdout: rendered.stdout,
                stderr: rendered.stderr,
                out_path: String::new(),
            }
        }
        Err(CallError::Timeout) => CallOutput::failed(
            EXIT_TIMEOUT,
            "timeout",
            format!(
                "kelir mcp: {}.{tool}: no answer within {} s; the call may or may not have \
                 happened, and it is not retried",
                config.name,
                config.timeout.as_secs()
            ),
        ),
        Err(CallError::Unavailable(message)) => CallOutput::failed(
            EXIT_UNAVAILABLE,
            "unavailable",
            format!("kelir mcp: {message}"),
        ),
        Err(error @ CallError::Rpc { .. }) => CallOutput::failed(
            EXIT_TOOL_ERROR,
            "error",
            format!("kelir mcp: {}.{tool}: {}", config.name, error.message()),
        ),
    };
    if write_out {
        let text = if output.stdout.is_empty() {
            &output.stderr
        } else {
            &output.stdout
        };
        let path = out_dir.join("out");
        if std::fs::create_dir_all(out_dir).is_ok() && std::fs::write(&path, text).is_ok() {
            output.out_path = crate::util::path_string(&path);
        }
    }
    output
}
