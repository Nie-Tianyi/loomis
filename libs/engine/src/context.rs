use std::sync::Arc;

use memory::SharedMemory;
use provider::LLMClient;
use tools::ToolRegistry;

use crate::hooks::AgentHook;

/// Configuration and dependencies for an [`Agent`](crate::Agent).
pub struct EngineContext {
    /// LLM provider implementation.
    pub llm: Box<dyn LLMClient>,
    /// Shared conversation memory.
    pub memory: SharedMemory,
    /// Tool registry (shared ownership).
    pub tools: Arc<ToolRegistry>,
    /// Lifecycle hooks (optional).
    pub hooks: Vec<Box<dyn AgentHook>>,
    /// Model name to send in API requests.
    pub model: String,
    /// Safety cap — maximum loop iterations before returning an error.
    pub max_steps: usize,
    /// Maximum retry attempts for transient failures.
    pub max_retries: usize,
    /// Whether to use SSE streaming.
    pub streaming: bool,
}
