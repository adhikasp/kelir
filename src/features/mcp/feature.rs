//! The MCP feature: broker lifecycle, the `kelir mcp` internal verbs, and channel
//! notifications. The broker owns servers; this feature owns the *log-side* half of a
//!
//! The session implements `McpCore` for the narrow set of core operations this feature
//! needs (record, sync, gate-run logging, disabled tools, live-call ids, turn). The feature
//! never holds the event writer.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::core::boundary::Feature;
use crate::ipc::{Incoming, Request, Response};
use crate::log::event::{EventBody, Payload};

struct McpPending {
    reply: oneshot::Sender<Response>,
}

/// The core operations the MCP feature needs. `Session` implements this; the feature
/// cannot see the writer or the rest of the loop.
pub trait McpCore {
    fn record(&mut self, body: EventBody) -> Result<crate::log::event::LogEvent, String>;
    fn sync(&mut self);
    fn disabled(&self) -> &std::collections::BTreeSet<String>;
    fn live_calls(&self) -> &[String];
    fn turn(&self) -> u32;
    fn gates(&self) -> Option<Arc<crate::features::gates::Runner>>;
    fn inbox(&self) -> Option<mpsc::UnboundedSender<Incoming>>;
}

/// The MCP feature's own state, moved out of `Session`.
pub struct Mcp {
    pub broker: crate::features::mcp::broker::Broker,
    pending: HashMap<String, McpPending>,
    /// Channel messages that arrived while a tool batch was settling; the next boundary
    /// writes them, so a call's results stay contiguous.
    channel_queue: Vec<(String, String)>,
}

impl Mcp {
    pub fn new(broker: crate::features::mcp::broker::Broker) -> Self {
        Self {
            broker,
            pending: HashMap::new(),
            channel_queue: Vec::new(),
        }
    }

    pub fn attach(&mut self, tx: mpsc::UnboundedSender<Incoming>) {
        self.broker.attach(tx);
    }

    /// Route a `kelir mcp` verb: internal settle/notify, or a client call.
    pub fn verb<C: McpCore>(
        &mut self,
        core: &mut C,
        request: Request,
        reply: oneshot::Sender<Response>,
    ) {
        match request.verb.as_str() {
            crate::features::mcp::broker::VERB_SETTLE
            | crate::features::mcp::broker::VERB_NOTIFY => {
                let internal = request.from == crate::features::mcp::broker::FROM_INTERNAL
                    && request.args.get("key").and_then(Value::as_str) == Some(self.broker.key());
                if !internal {
                    let _ = reply.send(Response::error(format!(
                        "{}: an internal verb",
                        request.verb
                    )));
                } else if request.verb == crate::features::mcp::broker::VERB_SETTLE {
                    self.settle(core, &request.args);
                    let _ = reply.send(Response::error("settled"));
                } else {
                    self.notify(core, &request.args);
                    let _ = reply.send(Response::error("noted"));
                }
            }
            _ => self.request(core, request, reply),
        }
    }

    /// and hand the result back through the inbox for the loop to settle.
    fn request<C: McpCore>(
        &mut self,
        core: &mut C,
        request: Request,
        reply: oneshot::Sender<Response>,
    ) {
        use crate::features::mcp::broker as broker_mod;
        let args = request.args;
        if args.get("op").and_then(Value::as_str) != Some("call") {
            let broker = self.broker.clone();
            tokio::spawn(async move {
                let _ = reply.send(broker.read_op(&args).await);
            });
            return;
        }
        let text = |key: &str| {
            args.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let (server, tool) = (text("server"), text("tool"));
        if !self.broker.has_server(&server) {
            let _ = reply.send(Response::ok(
                broker_mod::VERB,
                0,
                "now",
                self.broker.unknown_server(&server),
            ));
            return;
        }
        let arguments = args.get("args").cloned().unwrap_or_else(|| json!({}));
        let envelope = args.get("envelope").and_then(Value::as_bool) == Some(true);
        let mcp_id = format!("mcp_{}", ulid::Ulid::generate().to_string().to_lowercase());
        if let Err(message) = core.record(crate::log::event::call_start(
            &mcp_id,
            "mcp",
            crate::log::event::CallInfo::mcp(
                &server,
                &tool,
                Payload::value(crate::features::mcp::loggable_args(&arguments)),
            ),
        )) {
            let _ = reply.send(Response::error(message));
            return;
        }
        core.sync();
        self.pending.insert(mcp_id.clone(), McpPending { reply });
        let Some(inbox) = core.inbox() else {
            let output = crate::features::mcp::CallOutput::failed(
                crate::features::mcp::render::EXIT_UNAVAILABLE,
                "unavailable",
                "kelir mcp: the session is not serving MCP calls",
            );
            self.settle(core, &json!({"mcp_id": mcp_id, "output": output}));
            return;
        };
        // `tools` can turn off one tool (`mcp:<server>.<tool>`) or a whole
        // server (`mcp:<server>`); the call is still logged, and refused.
        if core.disabled().contains(&format!("mcp:{server}.{tool}"))
            || core.disabled().contains(&format!("mcp:{server}"))
        {
            let output = crate::features::mcp::CallOutput::failed(
                crate::features::mcp::render::EXIT_TOOL_ERROR,
                "disabled",
                format!("kelir mcp: {server}.{tool}: disabled by user"),
            );
            self.settle(core, &json!({"mcp_id": mcp_id, "output": output}));
            return;
        }
        let broker = self.broker.clone();
        let gates = core.gates();
        let turn = core.turn();
        tokio::spawn(async move {
            let (output, runs) = gated_mcp_call(
                gates.as_deref(),
                &broker,
                (&server, &tool),
                arguments,
                envelope,
                &mcp_id,
                turn,
            )
            .await;
            let args = json!({
                "key": broker.key(),
                "mcp_id": mcp_id,
                "output": output,
                "gate_runs": runs,
            });
            let (settle_reply, _dropped) = oneshot::channel();
            let _ = inbox.send(Incoming {
                request: Request::new(
                    crate::features::mcp::broker::VERB_SETTLE,
                    args,
                    crate::features::mcp::broker::FROM_INTERNAL,
                ),
                reply: settle_reply,
                action: None,
            });
        });
    }

    fn settle<C: McpCore>(&mut self, core: &mut C, args: &Value) {
        let Some(id) = args.get("mcp_id").and_then(Value::as_str) else {
            return;
        };
        let Some(pending) = self.pending.remove(id) else {
            return;
        };
        let output: crate::features::mcp::CallOutput = args
            .get("output")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_else(|| {
                crate::features::mcp::CallOutput::failed(
                    crate::features::mcp::render::EXIT_UNAVAILABLE,
                    "unavailable",
                    "kelir mcp: the broker lost the result",
                )
            });
        // The gates that ran on this call (they ran off the loop, so the loop
        // is the one that writes them down).
        if let Some(runs) = args.get("gate_runs").cloned().and_then(|value| {
            serde_json::from_value::<Vec<crate::features::gates::Run>>(value).ok()
        }) {
            for run in runs {
                let _ = core.record(EventBody::Gate {
                    gate: run.gate.clone(),
                    event: run.event.clone(),
                    decision: run.decision.clone(),
                    reason: run.reason.clone(),
                    ms: run.ms,
                    rewritten: run.rewritten,
                    error: run.error.clone(),
                });
            }
        }
        let recorded = core.record(crate::log::event::call_settle(
            id,
            "mcp",
            &output.outcome,
            crate::log::event::CallSettle::default()
                .exit_code(Some(output.exit_code))
                .bytes(output.bytes)
                .out_path(&output.out_path),
        ));
        core.sync();
        let answer = match recorded {
            Ok(_) => Response::ok(
                crate::features::mcp::broker::VERB,
                0,
                "now",
                serde_json::to_value(&output).unwrap_or(Value::Null),
            ),
            Err(message) => Response::error(message),
        };
        let _ = pending.reply.send(answer);
    }

    /// An inbound notification from a server. A channel message becomes a `source`
    /// event (kind `channel`) at the next safe boundary.
    fn notify<C: McpCore>(&mut self, core: &mut C, args: &Value) {
        let server = args
            .get("server")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let method = args.get("method").and_then(Value::as_str).unwrap_or("");
        let params = args.get("params").cloned().unwrap_or(Value::Null);
        if method == "kelir/unavailable" {
            let why = params
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let _ = core.record(EventBody::Error {
                location: "mcp".into(),
                message: format!("channel server `{server}` is unavailable: {why}"),
                retryable: false,
                status: None,
                attempt: None,
                retry_in_ms: None,
            });
            core.sync();
            return;
        }
        if !method.starts_with("notifications/claude/channel") {
            return;
        }
        let mut text = match params.get("content").and_then(Value::as_str) {
            Some(text) => text.to_string(),
            None => params.to_string(),
        };
        if let Some(meta) = params.get("meta").filter(|meta| !meta.is_null()) {
            text.push_str(&format!("\n[meta {meta}]"));
        }
        self.channel_queue.push((server, text));
        // Mid-batch the queue waits (a call's results stay contiguous); any
        // other time it is written now.
        if core.live_calls().is_empty() {
            self.flush(core);
        }
    }

    /// Write queued channel messages through the core's single writer.
    pub fn flush<C: McpCore>(&mut self, core: &mut C) {
        if self.channel_queue.is_empty() {
            return;
        }
        for (server, text) in std::mem::take(&mut self.channel_queue) {
            let _ = core.record(EventBody::Source {
                kind: "channel".into(),
                name: server,
                text: Some(Payload::text(text)),
                path: None,
            });
        }
        core.sync();
    }

    pub async fn shutdown(&self) {
        self.broker.shutdown().await;
    }

    pub fn servers(&self) -> Value {
        self.broker.servers()
    }
}

impl Feature for Mcp {}
/// Run the `before_tool`/`after_tool` gates around one MCP call. The gates run off
/// the loop; their log records are written by the loop when the call settles.
async fn gated_mcp_call(
    gates: Option<&crate::features::gates::Runner>,
    broker: &crate::features::mcp::broker::Broker,
    (server, tool): (&str, &str),
    mut arguments: Value,
    envelope: bool,
    mcp_id: &str,
    turn: u32,
) -> (
    crate::features::mcp::CallOutput,
    Vec<crate::features::gates::Run>,
) {
    use crate::features::gates::{Event, Verdict};
    let name = format!("mcp:{server}.{tool}");
    let mut runs = Vec::new();
    if let Some(gates) = gates.filter(|gates| gates.has(Event::BeforeTool, Some(&name))) {
        let payload = json!({"tool": name, "call_id": mcp_id, "input": arguments});
        let chain = gates
            .run_event(Event::BeforeTool, Some(&name), turn, payload, None)
            .await;
        runs.extend(chain.runs);
        match chain.verdict {
            Verdict::Deny { gate, reason, .. } => {
                let output = crate::features::mcp::CallOutput::failed(
                    crate::features::mcp::render::EXIT_TOOL_ERROR,
                    "denied",
                    crate::tools::denied_text(&gate, &reason),
                );
                return (output, runs);
            }
            Verdict::Rewrite(input) => arguments = input,
            Verdict::Allow => {}
        }
    }
    let mut output = broker
        .call(server, tool, arguments.clone(), envelope, mcp_id)
        .await;
    if let Some(gates) = gates.filter(|gates| gates.has(Event::AfterTool, Some(&name)))
        && matches!(output.outcome.as_str(), "ok" | "error")
    {
        let payload = json!({
            "tool": name,
            "call_id": mcp_id,
            "input": arguments,
            "result": output.stdout,
            "outcome": output.outcome,
            "exit_code": output.exit_code,
        });
        let chain = gates
            .run_event(Event::AfterTool, Some(&name), turn, payload, None)
            .await;
        runs.extend(chain.runs);
        if let Verdict::Rewrite(value) = chain.verdict
            && let Some(result) = value.get("result").and_then(Value::as_str)
        {
            output.stdout = result.to_string();
            output.bytes = output.stdout.len() as u64;
        }
    }
    (output, runs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    struct Core {
        events: Vec<EventBody>,
        calls: Vec<String>,
        disabled: BTreeSet<String>,
        turn: u32,
    }

    impl McpCore for Core {
        fn record(&mut self, body: EventBody) -> Result<crate::log::event::LogEvent, String> {
            self.events.push(body);
            Ok(crate::log::event::LogEvent {
                seq: self.events.len() as u64,
                ts: "2026-01-01T00:00:00Z".into(),
                body: self.events.last().unwrap().clone(),
            })
        }
        fn sync(&mut self) {}
        fn disabled(&self) -> &BTreeSet<String> {
            &self.disabled
        }
        fn live_calls(&self) -> &[String] {
            &self.calls
        }
        fn turn(&self) -> u32 {
            self.turn
        }
        fn gates(&self) -> Option<Arc<crate::features::gates::Runner>> {
            None
        }
        fn inbox(&self) -> Option<mpsc::UnboundedSender<Incoming>> {
            None
        }
    }

    fn broker() -> crate::features::mcp::broker::Broker {
        crate::features::mcp::broker::Broker::with_servers(
            Default::default(),
            None,
            std::env::temp_dir(),
            std::env::temp_dir(),
            "ses_test".into(),
        )
    }

    #[test]
    fn channel_notifications_wait_for_a_boundary_when_a_call_is_live() {
        let mut mcp = Mcp::new(broker());
        let mut core = Core {
            events: Vec::new(),
            calls: vec!["call_1".into()],
            disabled: BTreeSet::new(),
            turn: 1,
        };
        let args = json!({
            "server": "channel",
            "method": "notifications/claude/channel",
            "params": {"content": "wake up"}
        });
        mcp.notify(&mut core, &args);
        assert!(
            core.events.is_empty(),
            "a live batch keeps the queue waiting"
        );
        core.calls.clear();
        mcp.flush(&mut core);
        assert_eq!(core.events.len(), 1);
        assert!(
            matches!(&core.events[0], EventBody::Source { kind, text, .. }
            if kind == "channel" && format!("{text:?}").contains("wake up"))
        );
    }

    #[test]
    fn an_unavailable_channel_is_logged_as_an_error() {
        let mut mcp = Mcp::new(broker());
        let mut core = Core {
            events: Vec::new(),
            calls: Vec::new(),
            disabled: BTreeSet::new(),
            turn: 1,
        };
        mcp.notify(
            &mut core,
            &json!({
                "server": "channel",
                "method": "kelir/unavailable",
                "params": {"message": "gone"}
            }),
        );
        assert!(
            matches!(&core.events[0], EventBody::Error { location, message, .. }
            if location == "mcp" && message.contains("gone"))
        );
    }
}
