use engine::{AgentError, AgentHook};
use provider::ToolCall;
use tokio::sync::mpsc;

/// Events sent to a UI frontend via MPSC channel.
#[derive(Debug, Clone)]
pub enum UiEvent {
    Thinking,
    ToolCalled(String),
    Finished(String),
}

/// Hook that forwards lifecycle events to a UI via an MPSC channel.
pub struct UiStreamHook {
    pub tx: mpsc::Sender<UiEvent>,
}

impl AgentHook for UiStreamHook {
    fn on_llm_start(&self, _session_id: &str) {
        let _ = self.tx.try_send(UiEvent::Thinking);
    }

    fn before_tool_call(&self, _session_id: &str, tool: &ToolCall) -> Result<(), AgentError> {
        let _ = self
            .tx
            .try_send(UiEvent::ToolCalled(tool.function.name.clone()));
        Ok(())
    }
}
