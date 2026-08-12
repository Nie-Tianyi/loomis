//! Concrete [`AgentHook`](agent_oxide::engine::AgentHook) implementations.

mod plan_mode_hook;
mod profile_hook;
mod skill_hook;
mod system_prompt_hook;
mod todo_hook;

// Re-export the shared System-message placement helper from the hooks crate.
pub use agent_oxide::hooks::insert_before_history;

pub use agent_oxide::observability::ObservabilityHook;
pub use plan_mode_hook::{PlanModeHook, PlanModeState};
pub use profile_hook::ProfileHook;
pub use agent_oxide::sandbox::SandboxHook;
pub use skill_hook::SkillHook;
pub(crate) use system_prompt_hook::SYSPROMPT_MARKER;
pub use system_prompt_hook::SystemPromptHook;
pub use todo_hook::TodoListHook;
