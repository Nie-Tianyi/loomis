use std::sync::{Mutex, OnceLock};

use engine::{AgentError, AgentHook};
use provider::ToolCall;
use tokio::sync::mpsc;

use crate::app::HookEvent;

/// Hook that requires user approval before executing **any** shell command.
///
/// # Mechanism
///
/// The TUI cannot display prompts from within a synchronous hook because the
/// hook runs inside the agent's tokio task while the TUI owns the terminal.
/// Instead this hook uses a two-channel handshake:
///
/// 1. Send a [`HookEvent::ShellApprovalRequested`] through `hook_tx` so
///    the TUI renders an inline confirmation prompt.
/// 2. Block on `approval_rx` (a rendezvous [`std::sync::mpsc::SyncChannel`])
///    until the TUI thread posts the user's answer.
///
/// The `agent_tx` is a [`OnceLock`] because the channel is created in
/// [`build_coding_agent`] but the sender half isn't available until the
/// TUI event loop starts.
pub struct DangerousCommandApprovalHook {
    /// Sends shell approval events to the TUI. Initialised after construction
    /// via [`set_hook_tx`].
    hook_tx: OnceLock<mpsc::UnboundedSender<HookEvent>>,
    /// Blocking receive for the user's approval decision.
    approval_rx: Mutex<std::sync::mpsc::Receiver<bool>>,
}

impl DangerousCommandApprovalHook {
    /// Creates the hook and returns the sender half through which the
    /// agent handler signals the user's approval decision.
    ///
    /// The caller must pass the returned sender to
    /// [`agent_handler`](super::super::tui::event::agent_handler) so it can
    /// respond to [`TuiCommand::ShellConfirmation`].
    pub fn new() -> (Self, std::sync::mpsc::SyncSender<bool>) {
        // Rendezvous channel — send blocks until recv is ready.
        let (tx, rx) = std::sync::mpsc::sync_channel::<bool>(0);
        (
            Self {
                hook_tx: OnceLock::new(),
                approval_rx: Mutex::new(rx),
            },
            tx,
        )
    }

    /// Called by the TUI event loop once the `hook_tx` channel is created.
    pub fn set_hook_tx(&self, tx: mpsc::UnboundedSender<HookEvent>) {
        let _ = self.hook_tx.set(tx);
    }
}

impl AgentHook for DangerousCommandApprovalHook {
    fn before_tool_call(&self, _session_id: &str, tool: &ToolCall) -> Result<(), AgentError> {
        if tool.function.name != "shell" {
            return Ok(());
        }

        // Parse the command from the tool arguments.
        let command = parse_shell_command(&tool.function.arguments);

        // Send approval request to the TUI.
        if let Some(tx) = self.hook_tx.get() {
            let _ = tx.send(HookEvent::ShellApprovalRequested {
                tool_call_id: tool.id.clone(),
                command: command.clone(),
            });
        }

        // Block until the TUI responds.
        let approved = self.approval_rx.lock().unwrap().recv().unwrap_or(false);

        if !approved {
            return Err(AgentError::ToolRejected {
                name: "shell".into(),
                reason: "User denied shell command execution".into(),
            });
        }

        Ok(())
    }
}

/// Extracts the `command` field from shell tool arguments JSON.
fn parse_shell_command(args: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(args) {
        v.get("command")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string()
    } else {
        args.to_string()
    }
}
