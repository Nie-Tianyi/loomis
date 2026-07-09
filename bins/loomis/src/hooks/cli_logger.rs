use engine::{AgentError, AgentHook};
use provider::ToolCall;

/// Hook that prints progress information to the terminal.
pub struct CliLoggerHook;

impl AgentHook for CliLoggerHook {
    fn on_llm_start(&self, _session_id: &str) {
        eprintln!("\u{23f3} Agent thinking...");
    }

    fn before_tool_call(&self, _session_id: &str, tool: &ToolCall) -> Result<(), AgentError> {
        eprintln!(
            "\u{1f527} Executing tool: {} | args: {}",
            tool.function.name, tool.function.arguments
        );
        Ok(())
    }
}
