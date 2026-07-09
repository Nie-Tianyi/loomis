use async_trait::async_trait;
use engine::{AgentError, AgentHook};
use provider::ToolCall;

/// Hook that prompts the user before executing shell commands.
///
/// Blocks the agent loop until the user approves (Y) or denies (n).
pub struct DangerousCommandApprovalHook;

#[async_trait]
impl AgentHook for DangerousCommandApprovalHook {
    async fn before_tool_call(&self, _session_id: &str, tool: &ToolCall) -> Result<(), AgentError> {
        if tool.function.name == "shell" {
            // Parse the command from arguments.
            let cmd = if let Ok(v) =
                serde_json::from_str::<serde_json::Value>(&tool.function.arguments)
            {
                v.get("command")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string()
            } else {
                tool.function.arguments.clone()
            };

            // Check for dangerous patterns.
            let dangerous =
                cmd.contains("rm -rf") || cmd.contains("drop table") || cmd.contains("format C:");

            if dangerous {
                eprintln!("\u{26a0}\u{fe0f}  Warning: Agent wants to run a dangerous command:");
                eprintln!("> {cmd}");
                eprint!("Allow execution? (y/N): ");

                use std::io::Write;
                let _ = std::io::stdout().flush();

                let mut input = String::new();
                if std::io::stdin().read_line(&mut input).is_err() {
                    return Err(AgentError::ToolRejected {
                        name: "shell".into(),
                        reason: "Failed to read user input".into(),
                    });
                }

                if input.trim().to_lowercase() != "y" {
                    return Err(AgentError::ToolRejected {
                        name: "shell".into(),
                        reason: "User denied dangerous command".into(),
                    });
                }
            }
        }
        Ok(())
    }
}
