//! The feature registry. The core owns one `Features` and calls its hooks by name of
//! boundary; it never names a member. Dispatch is static, in the documented order.

pub mod children;
pub mod children_ops;
pub mod gates;
pub mod mcp;
pub mod models;
pub mod sources;

use crate::core::boundary::{Cx, Feature};

pub(crate) struct Features {
    pub(crate) models: models::Models,
    pub(crate) mcp: Option<mcp::Mcp>,
    pub(crate) gates: Option<gates::Gates>,
    pub(crate) sources: sources::Sources,
}

impl Features {
    pub(crate) async fn before_step(&mut self, cx: &mut Cx<'_>) {
        Feature::before_step(&mut self.models, cx).await;
        if let Some(mcp) = self.mcp.as_mut() {
            Feature::before_step(mcp, cx).await;
        }
    }

    /// Sources are checked after compaction, before the request: a `source`
    /// change lands at the tail and must not be folded into the summary.
    pub(crate) fn check_sources(&mut self, cx: &mut Cx<'_>) -> Result<(), String> {
        let sources = &mut self.sources;
        Feature::check_sources(sources, cx)
    }

    pub(crate) fn status(&self, out: &mut serde_json::Map<String, serde_json::Value>) {
        Feature::status(&self.models, out);
        Feature::status(&self.sources, out);
    }

    /// Status fields owned by features that also need a view of the session.
    /// The core calls this once, after `status`, and never constructs these
    /// values itself.
    pub(crate) fn status_extended(
        &self,
        session: &crate::core::session::types::Session,
        out: &mut serde_json::Map<String, serde_json::Value>,
    ) {
        use serde_json::{Value, json};
        // MCP servers: name, state, tool count and channel flag only. Never
        // config, env, descriptions or server stderr (those can hold secrets).
        let servers = self.mcp.as_ref().expect("mcp feature").servers();
        let mcp: Vec<Value> = servers["servers"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| {
                let state = s["state"].as_str().unwrap_or("");
                // A `failed: ...` state carries server text; keep the word only.
                let state = if state.starts_with("failed") { "failed" } else { state };
                json!({"name": s["name"], "state": state, "tools": s["tools"], "channel": s["channel"]})
            })
            .collect();
        out.insert("mcp".into(), Value::Array(mcp));
        let gates = self.gates.as_ref().expect("gates feature").status(session);
        out.insert("gates".into(), gates);
        out.insert("children".into(), json!(session.live_children()));
        out.insert("child_list".into(), session.child_list());
    }
}

/// Verbs owned by features that must be answered before the core parses them.
/// `Ok(())` means the verb was handled; `Err` hands the request back unchanged.
pub(crate) fn early_feature_verb(
    session: &mut crate::core::session::types::Session,
    request: crate::ipc::Request,
    reply: tokio::sync::oneshot::Sender<crate::ipc::Response>,
) -> Result<
    (),
    (
        crate::ipc::Request,
        tokio::sync::oneshot::Sender<crate::ipc::Response>,
    ),
> {
    if crate::mcp::broker::is_mcp_verb(&request.verb) {
        session.mcp_do(|mcp, session| mcp.verb(session, request, reply));
        return Ok(());
    }
    if request.verb == crate::child::VERB || request.verb == "brief" {
        session.child_or_brief(request, reply);
        return Ok(());
    }
    Err((request, reply))
}

/// Flush feature work at a verb boundary before the core applies pending verbs.
pub(crate) fn flush_features(session: &mut crate::core::session::types::Session) {
    session.mcp_do(|mcp, session| mcp.flush(session));
}
