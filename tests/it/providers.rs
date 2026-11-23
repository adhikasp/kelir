//! Integration tests filed by feature: `providers`.

#[path = "providers_auth.rs"]
mod providers_auth;

#[path = "providers_limits.rs"]
mod providers_limits;

#[path = "providers_retry.rs"]
mod providers_retry;

#[path = "providers_status.rs"]
mod providers_status;

#[path = "providers_basics.rs"]
mod providers_basics;

#[path = "providers_windows.rs"]
mod providers_windows;

#[path = "providers_secrets.rs"]
mod providers_secrets;

#[path = "providers_governor.rs"]
mod providers_governor;

#[path = "providers_status_edges.rs"]
mod providers_status_edges;
