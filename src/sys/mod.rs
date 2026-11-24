//! One child-process runner: the only way `src/` starts a process.
//! The bg shim is a documented exception: it must outlive the session and is a detached
//! `kelir bg-shim`; it still builds its environment from [`runner::scrubbed_env`] when the
//! starter gives it one, and reuses the same grouping through [`runner::start`].
pub mod atomic;
pub mod joblock;
pub mod perm;
pub mod proc;
pub mod runner;
pub mod shell;
pub mod signals;
pub mod trust;
