//! STEER-6 end-to-end: the retro's loop improvements, driven through the real
//! binary against the scripted mock.
//!
//! * **F2** a 40 KB heredoc is not cut (it ran from a file, and its line count
//!   proves every line arrived);
//! * **F1** `reasoning_effort` reaches the request body and is recorded in
//!   `session_started`;
//! * **F3** `edits` applies several hunks in one call, or none;
//! * **F4** `parallel_tool_calls` is set only for a model that advertises it;
//! * **F5** `compact_at_tokens` compacts a big window early.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]
