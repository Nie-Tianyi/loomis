use std::collections::HashSet;
use std::sync::Arc;

use memory::{DEFAULT_COMPACTABLE_TOOLS, DEFAULT_KEEP_RECENT_TOOL_OUTPUTS, SharedMemory};
use provider::LLMClient;
use tools::ToolRegistry;

use crate::hooks::AgentHook;

/// Configuration and dependencies for an [`Agent`](crate::Agent).
///
/// All fields are public for direct construction by advanced users.
/// Most users should prefer the builder API via [`EngineContext::builder`]
/// or the even simpler [`Agent::builder`](crate::Agent::builder).
pub struct EngineContext<C: LLMClient> {
    /// LLM provider implementation.
    pub llm: C,
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
    // ── Tool output compaction (MicroCompact) ──────────────────────────
    /// Whether to compact old tool outputs before sending context to the LLM.
    /// When true, [`Memory::to_compact_context_vec`] is used instead of
    /// [`Memory::to_context_vec`].
    pub compact_tool_outputs: bool,
    /// Number of recent tool outputs to preserve when compacting.
    pub keep_recent_tool_outputs: usize,
    /// Tool names eligible for output compaction.
    pub compactable_tool_names: HashSet<String>,
    // ── Full LLM compaction ────────────────────────────────────────────
    /// Model to use for full-memory summarisation compaction.
    /// When `Some`, the agent checks for [`memory::CompactSignal::NeedsCompact`]
    /// after each turn and triggers an LLM summarisation pass.  Use a cheap /
    /// fast model here (e.g. `"deepseek-v4-flash"`).
    /// When `None`, only tool-output (micro) compaction runs.
    pub compact_model: Option<String>,
}

impl<C: LLMClient> EngineContext<C> {
    /// Create a new [`EngineContextBuilder`] with the four **required**
    /// dependencies.
    ///
    /// All other fields use sensible defaults:
    ///
    /// | Field | Default |
    /// |-------|---------|
    /// | `hooks` | empty |
    /// | `max_steps` | `50` |
    /// | `max_retries` | `3` |
    /// | `streaming` | `true` |
    /// | `compact_tool_outputs` | `false` |
    /// | `keep_recent_tool_outputs` | `5` |
    /// | `compactable_tool_names` | empty |
    /// | `compact_model` | `None` |
    ///
    /// # Example
    ///
    /// ```ignore
    /// let ctx = EngineContext::builder(client, memory, registry, "deepseek-v4")
    ///     .hook(my_hook)
    ///     .max_steps(100)
    ///     .with_micro_compact(5, my_compactable_tools)
    ///     .with_macro_compact("deepseek-v4-flash")
    ///     .build();
    /// let agent = Agent::new(ctx);
    /// ```
    pub fn builder(
        llm: C,
        memory: SharedMemory,
        tools: Arc<ToolRegistry>,
        model: impl Into<String>,
    ) -> EngineContextBuilder<C> {
        EngineContextBuilder {
            llm,
            memory,
            tools,
            model: model.into(),
            hooks: Vec::new(),
            max_steps: 50,
            max_retries: 3,
            streaming: true,
            compact_tool_outputs: false,
            keep_recent_tool_outputs: 5,
            compactable_tool_names: HashSet::new(),
            compact_model: None,
        }
    }

    /// Populate the tool-output compaction fields with sensible defaults.
    ///
    /// Call this (or set the fields manually) before constructing an
    /// [`Agent`](crate::Agent) if you have `compact_tool_outputs: true`.
    pub fn with_compact_defaults(mut self) -> Self {
        self.keep_recent_tool_outputs = DEFAULT_KEEP_RECENT_TOOL_OUTPUTS;
        self.compactable_tool_names = DEFAULT_COMPACTABLE_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect();
        self
    }
}

// ── EngineContextBuilder ────────────────────────────────────────────────────

/// Fluent builder for [`EngineContext`].
///
/// Created via [`EngineContext::builder`].  Call [`build`](Self::build) to
/// produce the final [`EngineContext`].
///
/// This is the **advanced** API — most users should prefer
/// [`Agent::builder`](crate::Agent::builder) which wraps this builder with
/// convenient defaults (auto-created memory, tool registration, system
/// prompt seeding).
pub struct EngineContextBuilder<C: LLMClient> {
    pub(crate) llm: C,
    pub(crate) memory: SharedMemory,
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) model: String,
    pub(crate) hooks: Vec<Box<dyn AgentHook>>,
    pub(crate) max_steps: usize,
    pub(crate) max_retries: usize,
    pub(crate) streaming: bool,
    pub(crate) compact_tool_outputs: bool,
    pub(crate) keep_recent_tool_outputs: usize,
    pub(crate) compactable_tool_names: HashSet<String>,
    pub(crate) compact_model: Option<String>,
}

impl<C: LLMClient> EngineContextBuilder<C> {
    /// Register a single lifecycle hook.
    ///
    /// Hooks are called in the order they are registered.
    pub fn hook(mut self, hook: impl AgentHook + 'static) -> Self {
        self.hooks.push(Box::new(hook));
        self
    }

    /// Register multiple lifecycle hooks at once.
    pub fn hooks(mut self, hooks: impl IntoIterator<Item = Box<dyn AgentHook>>) -> Self {
        self.hooks.extend(hooks);
        self
    }

    /// Override the default maximum loop iterations (default: `50`).
    ///
    /// When the agent reaches this many ReAct loop steps it returns
    /// [`AgentError::MaxStepsReached`](crate::AgentError::MaxStepsReached).
    pub fn max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
        self
    }

    /// Override the default maximum retry attempts (default: `3`).
    ///
    /// Transient LLM provider failures are retried with exponential
    /// backoff up to this many times.
    pub fn max_retries(mut self, max_retries: usize) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Enable or disable SSE streaming (default: `true`).
    ///
    /// When enabled the agent uses `LLMClient::stream()` and emits
    /// [`AgentEvent::Token`] and [`AgentEvent::ToolCallArgsDelta`] events
    /// in real time.  When disabled it uses `LLMClient::generate()` and
    /// emits a single [`AgentEvent::Token`] with the full response.
    pub fn streaming(mut self, streaming: bool) -> Self {
        self.streaming = streaming;
        self
    }

    /// Enable or disable tool-output compaction (default: `false`).
    ///
    /// When enabled, use [`keep_recent_tool_outputs`](Self::keep_recent_tool_outputs)
    /// and [`compactable_tool_names`](Self::compactable_tool_names) to
    /// configure which tools and how many recent outputs to preserve.
    pub fn compact_tool_outputs(mut self, enabled: bool) -> Self {
        self.compact_tool_outputs = enabled;
        self
    }

    /// Number of recent tool outputs to preserve per tool when
    /// [`compact_tool_outputs`](Self::compact_tool_outputs) is enabled
    /// (default: `5`).
    pub fn keep_recent_tool_outputs(mut self, n: usize) -> Self {
        self.keep_recent_tool_outputs = n;
        self
    }

    /// Set of tool names eligible for output compaction (default: empty).
    pub fn compactable_tool_names(mut self, names: HashSet<String>) -> Self {
        self.compactable_tool_names = names;
        self
    }

    /// Set the model for macro-compaction.
    ///
    /// When `Some`, full LLM summarisation is enabled.  Use a cheap /
    /// fast model (e.g. `"deepseek-v4-flash"`).  When `None`, only
    /// micro-compaction runs.
    pub fn compact_model(mut self, model: impl Into<String>) -> Self {
        self.compact_model = Some(model.into());
        self
    }

    /// Enable **micro-compaction** of tool outputs (shorthand).
    ///
    /// Equivalent to calling:
    /// ```ignore
    /// .compact_tool_outputs(true)
    /// .keep_recent_tool_outputs(keep_recent)
    /// .compactable_tool_names(compactable_tools)
    /// ```
    pub fn with_micro_compact(
        mut self,
        keep_recent: usize,
        compactable_tools: HashSet<String>,
    ) -> Self {
        self.compact_tool_outputs = true;
        self.keep_recent_tool_outputs = keep_recent;
        self.compactable_tool_names = compactable_tools;
        self
    }

    /// Enable **macro-compaction** (full LLM summarisation) — shorthand.
    ///
    /// Equivalent to calling `.compact_model(model)`.
    pub fn with_macro_compact(mut self, model: impl Into<String>) -> Self {
        self.compact_model = Some(model.into());
        self
    }

    /// Consume the builder and produce an [`EngineContext`].
    pub fn build(self) -> EngineContext<C> {
        EngineContext {
            llm: self.llm,
            memory: self.memory,
            tools: self.tools,
            model: self.model,
            hooks: self.hooks,
            max_steps: self.max_steps,
            max_retries: self.max_retries,
            streaming: self.streaming,
            compact_tool_outputs: self.compact_tool_outputs,
            keep_recent_tool_outputs: self.keep_recent_tool_outputs,
            compactable_tool_names: self.compactable_tool_names,
            compact_model: self.compact_model,
        }
    }
}
