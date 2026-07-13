//! Hook that persists the conversation to disk after each agent run.
//!
//! Implements `on_run_finish` to save the full conversation state
//! (JSON + Markdown) to the configured threads directory.  Fires for
//! both success and error outcomes — cancellation bypasses hooks
//! because the agent task is aborted by the TUI.
//!
//! Exit-time and ClearConversation saves remain in the TUI handler
//! because those are UI lifecycle events, not agent run events.

use std::path::PathBuf;

use engine::{AgentHook, RunOutcome};
use memory::{PersistenceConfig, SharedMemory};

/// Saves conversation to disk after every agent run completes.
pub struct PersistenceHook {
    workspace_root: PathBuf,
    config: PersistenceConfig,
}

impl PersistenceHook {
    pub fn new(workspace_root: PathBuf, config: PersistenceConfig) -> Self {
        Self {
            workspace_root,
            config,
        }
    }
}

impl AgentHook for PersistenceHook {
    fn on_run_finish(&self, _session_id: &str, _outcome: &RunOutcome, memory: &SharedMemory) {
        let mem = memory.read().expect("memory lock poisoned");
        let name = memory::default_thread_name(&self.workspace_root, &self.config);
        let _ = memory::save_conversation(&name, &self.workspace_root, &mem, &self.config);
    }
}
