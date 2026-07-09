//! Concrete [`AgentHook`](engine::AgentHook) implementations.

mod cli_logger;
mod sandbox_hook;
mod shell_approval;
mod ui_stream;

pub use cli_logger::CliLoggerHook;
pub use sandbox_hook::SandboxHook;
pub use shell_approval::DangerousCommandApprovalHook;
pub use ui_stream::{UiEvent, UiStreamHook};
