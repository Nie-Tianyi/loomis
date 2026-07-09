//! Loomis — concrete agent implementation with DeepSeek, tools, hooks, and TUI.

pub mod app;
pub mod compact;
pub mod hooks;
pub mod tools;
pub mod tui;

pub use app::{AgentKit, build_coding_agent};
pub use compact::compact_with_deepseek;
