use provider::{Message, ToolCall};

use crate::agent::AgentError;

/// Lifecycle hook for observing and intervening in agent execution.
///
/// All methods have default no-op implementations — implement only
/// the events you care about.
///
/// The `before_tool_call` hook can return `Err` to **block** execution
/// of a tool (e.g. user denied a dangerous shell command).
///
/// All methods are synchronous. If a hook needs to perform async work,
/// it should spawn a task internally or use blocking I/O.
#[allow(unused_variables)]
pub trait AgentHook: Send + Sync {
    /// Called when a new user input begins a full task run.
    fn on_run_start(&self, session_id: &str, user_input: &str) {}

    /// Called before sending a request to the LLM.
    fn on_llm_start(&self, session_id: &str) {}

    /// Called after receiving a response from the LLM.
    fn on_llm_end(&self, session_id: &str, response: &Message) {}

    /// Called before executing a tool.
    ///
    /// Return `Err(AgentError::ToolRejected)` to skip the tool and add
    /// the error message as the observation instead.
    fn before_tool_call(&self, session_id: &str, tool_call: &ToolCall) -> Result<(), AgentError> {
        Ok(())
    }

    /// Called after a tool has been executed, with its observation.
    fn after_tool_call(&self, session_id: &str, tool_call: &ToolCall, observation: &str) {}
}
