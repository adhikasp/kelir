//! The rewrite's one integration-test binary, one source file per feature.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

#[path = "support/mod.rs"]
mod support;

mod attachments;

mod bg;

mod buildinfra;

mod channel;

mod children;

mod cli;

mod gates;

mod hint;

mod jobs;

#[path = "jobs_status.rs"]
mod jobs_status;

#[path = "jobs_limits.rs"]
mod jobs_limits;

mod loop_;

mod mcp;

mod notify;

mod platform;

mod providers;

mod resume;

mod sources;

mod tools;

mod tui;

mod verbs;

mod p6d_windows;
