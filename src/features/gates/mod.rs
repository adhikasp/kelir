//! Gates: the only way a veto or a nudge enters the core.
//! A gate is an executable or a built-in Rust gate the session runs at one of six named events.
//! It gets the event as JSON and may answer `allow`, `deny` or `rewrite` as JSON on stdout (built-ins
//! return the same types in process).
//! Opinions ("verify before you claim", "check the note you edited") live in gates, never in the

pub mod config;
pub mod decision;
pub mod feature;
pub mod limits;
pub mod runner;

pub mod builtin;

pub use config::*;
pub use decision::*;
pub(crate) use feature::*;
pub use limits::*;
pub use runner::*;
