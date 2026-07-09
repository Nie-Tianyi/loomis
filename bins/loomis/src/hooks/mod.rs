//! Concrete [`AgentHook`](engine::AgentHook) implementations.

mod cli_logger;
mod shell_approval;
mod ui_stream;

pub use cli_logger::CliLoggerHook;
pub use shell_approval::DangerousCommandApprovalHook;
pub use ui_stream::{UiEvent, UiStreamHook};
