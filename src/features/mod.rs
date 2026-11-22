//! The feature registry. The core owns one `Features` and calls its hooks by name of
//! boundary; it never names a member. Dispatch is static, in the documented order.

pub mod models;

use crate::core::boundary::{Cx, Feature};

pub(crate) struct Features {
    pub(crate) models: models::Models,
}

impl Features {
    pub(crate) async fn before_step(&mut self, cx: &mut Cx<'_>) {
        Feature::before_step(&mut self.models, cx).await;
    }

    pub(crate) fn status(&self, out: &mut serde_json::Map<String, serde_json::Value>) {
        Feature::status(&self.models, out);
    }
}
