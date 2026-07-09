//! Sandbox components — command filtering, environment sanitization,
//! resource tracking, and audit logging.
//!
//! These are concrete implementations used by `ShellTool` and `SandboxHook`.
//! The configuration types live in `libs/tools/src/sandbox/` so they can
//! be shared with the `WorkspaceFs` sandbox.

pub mod audit_logger;
pub mod env_sanitizer;
pub mod resource_tracker;
pub mod shell_filter;
