//! Sandbox configuration types shared across the tool system.
//!
//! [`SandboxConfig`] defines all security policies — filesystem limits,
//! shell command filtering, resource quotas, and audit settings. It is
//! deserialised from a TOML file at startup and injected into every
//! sandbox component.

mod config;

pub use config::SandboxConfig;
