//! # Engine — Agent ReAct Loop
//!
//! Core agent loop with [`AgentHook`] lifecycle and streaming [`AgentEvent`]s.

mod agent;
mod context;
mod hooks;

pub use agent::{Agent, AgentError, AgentEvent};
pub use context::EngineContext;
pub use hooks::AgentHook;
