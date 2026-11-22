//! The feature registry. The core owns one `Features` and calls its hooks by name of
//! boundary; it never names a member. Dispatch is static, in the documented order.

pub mod mcp;
pub mod models;
pub mod sources;

use crate::core::boundary::{Cx, Feature};

pub(crate) struct Features {
    pub(crate) models: models::Models,
    pub(crate) mcp: Option<mcp::Mcp>,
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
}
