//! Integration tests filed by feature: `platform`.

#[path = "platform_security.rs"]
mod platform_security;

#[path = "platform_limits.rs"]
mod platform_limits;

#[path = "platform_ownership.rs"]
mod platform_ownership;

#[path = "platform_forgery.rs"]
mod platform_forgery;

#[path = "platform_process.rs"]
mod platform_process;
