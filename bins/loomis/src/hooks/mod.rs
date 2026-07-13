//! Concrete [`AgentHook`](engine::AgentHook) implementations.

mod persistence_hook;
mod sandbox_hook;
mod system_prompt_hook;

pub use engine::{ResponseRouter, next_request_id};
pub use persistence_hook::PersistenceHook;
pub use sandbox_hook::SandboxHook;
pub use system_prompt_hook::SystemPromptHook;
