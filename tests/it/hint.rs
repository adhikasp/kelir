//! Integration tests filed by feature: `hint`.

#[path = "hint_core.rs"]
pub(super) mod hint_core;

#[path = "hint_adversarial.rs"]
mod hint_adversarial;

#[path = "context_reads.rs"]
mod context_reads;

#[path = "context_metrics.rs"]
mod context_metrics;

#[path = "hint_herdr.rs"]
mod hint_herdr;

#[path = "hint_observe.rs"]
mod hint_observe;

#[path = "hint_herdr_adversarial.rs"]
mod hint_herdr_adversarial;

#[path = "hint_memory_adversarial.rs"]
mod hint_memory_adversarial;

#[path = "hint_headers.rs"]
mod hint_headers;
