//! Model capabilities and request shaping.
// These private loop modules expose crate-visible helpers to the feature modules.
#![allow(clippy::redundant_pub_crate)]

use super::*;

impl Session {
    // ---------------------------------------------------------- compaction

    /// The effort the first request will carry: the configured value, unless the provider's list says the
    /// model does not take the parameter.
    pub(crate) fn first_effort(&self) -> Option<String> {
        if !self.effort_allowed() {
            return None;
        }
        self.cfg
            .provider(&self.provider.provider_name)
            .ok()
            .and_then(|provider| provider.reasoning_effort)
    }

    /// Whether the loop may ask this model for several tool calls in one message: the provider's list when
    /// it has an entry, else the built-in family table, so a first session (cold or slow list) still asks a
    pub(crate) fn parallel_calls(&self) -> bool {
        crate::models::parallel_calls(
            self.features
                .models
                .models
                .as_ref()
                .map(|cache| cache.models.as_slice()),
            &self.provider.model,
        )
    }

    /// Whether `reasoning_effort` may go in the body: only where the provider advertises the parameter, or
    /// for a model the list says nothing about when the person configured an effort explicitly.
    pub(crate) fn effort_allowed(&self) -> bool {
        let explicit = self
            .cfg
            .provider(&self.provider.provider_name)
            .map(|provider| provider.reasoning_effort_explicit)
            .unwrap_or(false);
        crate::models::reasoning_effort_allowed(
            self.features
                .models
                .models
                .as_ref()
                .map(|cache| cache.models.as_slice()),
            &self.provider.model,
            explicit,
        )
    }

    /// The request for one step, with the two capability decisions applied.
    pub(crate) fn step_request(
        &self,
        messages: &[crate::chat::ChatMessage],
    ) -> provider::BuiltRequest {
        self.capabilities(self.provider.client.build_with(
            &self.provider.model,
            messages,
            &self.tool_specs(),
            self.parallel_calls(),
        ))
    }

    /// Apply what this provider/model actually takes: `reasoning_effort` only where the provider advertises
    /// it or the config asked for it on an unknown model, and nothing this session already saw a 400 for.
    pub(crate) fn capabilities(&self, request: provider::BuiltRequest) -> provider::BuiltRequest {
        let mut request = request;
        if !self.effort_allowed() {
            request = request.without_field("reasoning_effort");
        }
        for field in &self.provider.refused_fields {
            request = request.without_field(field);
        }
        request
    }

    /// The tool list sent to the provider: the specs for the names frozen at `session_started`, in that
    /// order.
    pub(crate) fn tool_specs(&self) -> Vec<Value> {
        let all = tools::specs();
        self.tools
            .names
            .iter()
            .filter_map(|name| {
                all.iter()
                    .find(|spec| spec["function"]["name"].as_str() == Some(name.as_str()))
                    .cloned()
            })
            .collect()
    }
}
