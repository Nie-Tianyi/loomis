//! # Engine — Agent ReAct Loop
//!
//! Core agent loop with [`AgentHook`] lifecycle and streaming [`AgentEvent`]s.

#![allow(async_fn_in_trait)]

mod agent;
mod context;
mod hooks;

// Re-export public error types so callers don't need the submodules.
mod error {
    pub use super::agent::AgentError;
}

pub use agent::{Agent, AgentEvent};
pub use context::EngineContext;
pub use error::AgentError;
pub use hooks::AgentHook;
