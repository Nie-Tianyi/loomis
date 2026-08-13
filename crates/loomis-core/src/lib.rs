//! # loomis-core
//!
//! UI-agnostic agent core for Loomis: DeepSeek agent, concrete tools, hooks,
//! sandbox wiring, persistence, and the runtime driver.
//!
//! Frontends (TUI, WebUI, GUI) depend on this crate only — never on
//! `agent_oxide` directly. The public surface is the [`Runtime`] façade plus
//! the re-exported event/state types a frontend needs to render a session.
//!
//! [`Runtime`]: crate::runtime::Runtime

mod app;
pub mod config;
pub mod hooks;
pub mod profile;
pub mod runtime;
pub mod tools;

mod shell_util;
mod user_shell;

// ── agent_oxide type re-exports ──────────────────────────────────────────
// Frontends consume these through loomis-core so they never depend on the
// framework crate directly. All are UI-agnostic framework types.

pub use agent_oxide::engine::{panic_message, AgentEvent, CallOrigin, InterventionResponse};
pub use agent_oxide::memory::{Memory, PendingHints, SharedMemory};
pub use agent_oxide::observability::TraceStore;
pub use agent_oxide::persistence::{sanitize_filename, PersistenceConfig, ThreadInfo};
pub use agent_oxide::provider::{Message, Role};
pub use agent_oxide::sandbox::SandboxConfig;
pub use agent_oxide::sandbox::shell_filter::{CommandVerdict, ShellFilter};
pub use agent_oxide::skills::{ActiveSkills, SkillRegistry};
pub use config::{CoreConfig, DEFAULT_FLASH_MODEL, DEFAULT_MODEL};
pub use hooks::PlanModeState;
pub use runtime::{ApproveOutcome, Runtime, RuntimeCommand, UiState};
pub use tools::{TodoItem, TODO_MARKER};

/// Whether `name` is a valid thread name — non-empty and unchanged by
/// sanitisation (no control characters, DOS-reserved names, or
/// `/\:*?"<>|`).
pub fn is_valid_thread_name(name: &str) -> bool {
    !name.is_empty() && name == sanitize_filename(name)
}
