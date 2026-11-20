//! kelir — a small, steerable coding-agent harness.
//!
//! One session is one OS process plus one append-only event log; every
//! request is rebuilt from `fold(events.jsonl)` so the loop is the same live
//! and on resume.

pub mod activity;
pub mod attachments;
pub mod bg;
pub mod chat;
pub mod cli;
pub mod commands;
pub mod compact;
pub mod config;
pub mod credentials;
pub mod ctl;
pub mod docs;
pub mod doctor;
pub mod editor;
pub mod envs;
pub mod event;
pub mod fold;
pub mod governor;
pub mod herdr;
pub mod ipc;
pub mod markdown;
pub mod models;
pub mod paths;
pub mod proc;
pub mod provider;
pub mod resume;
pub mod retry;
pub mod scrollback;
pub mod session;
pub mod shellcmd;
pub mod signals;
pub mod sources;
pub mod spill;
pub mod sse;
pub mod stream;
pub mod theme;
pub mod tools;
pub mod tools_view;
pub mod tui;
pub mod util;
pub mod verbs;

/// The crate version, as reported to providers and in the event log.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
