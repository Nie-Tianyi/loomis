use async_trait::async_trait;
use engine::{AgentError, AgentHook};
use provider::ToolCall;

/// Hook that prints progress information to the terminal.
pub struct CliLoggerHook;

#[async_trait]
impl AgentHook for CliLoggerHook {
    async fn on_llm_start(&self, _session_id: &str) {
        eprintln!("\u{23f3} Agent thinking...");
    }

    async fn before_tool_call(
        &self,
        _session_id: &str,
        tool: &ToolCall,
    ) -> Result<(), AgentError> {
        eprintln!("\u{1f527} Executing tool: {} | args: {}", tool.function.name, tool.function.arguments);
        Ok(())
    }
}
