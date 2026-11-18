//! kelir — a small, steerable coding-agent harness.
//!
//! One session is one OS process plus one append-only event log; every
//! request is rebuilt from `fold(events.jsonl)` so the loop is the same live
//! and on resume.

pub mod chat;
pub mod cli;
pub mod config;
pub mod event;
pub mod fold;
pub mod paths;
pub mod proc;
pub mod provider;
pub mod session;
pub mod spill;
pub mod sse;
pub mod stream;
pub mod tools;
pub mod util;

/// The crate version, as reported to providers and in the event log.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
