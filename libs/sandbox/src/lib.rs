//! Sandbox runtime components and security policy configuration.
//!
//! Provides concrete implementations for command filtering
//! ([`shell_filter`]), environment sanitization
//! ([`env_sanitizer`]), resource quota tracking
//! ([`resource_tracker`]), audit logging ([`audit_logger`]),
//! output encoding ([`encoding`]), the [`SandboxHook`]
//! ([`sandbox_hook`]) AgentHook, and the configuration
//! types ([`config`]) that drive them.

pub mod audit_logger;
pub mod config;
pub mod encoding;
pub mod env_sanitizer;
pub mod resource_tracker;
pub mod sandbox_hook;
pub mod shell_filter;

pub use config::{ConfigError, SandboxConfig};
pub use sandbox_hook::SandboxHook;
