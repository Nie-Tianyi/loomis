//! Builder for [`Agent`] — the primary entry point for most users.
//!
//! [`AgentBuilder`] provides a fluent API for assembling an [`Agent`] with
//! its LLM client, model, tools, memory, hooks, and compaction settings.
//!
//! # Quick start
//!
//! ```ignore
//! use engine::Agent;
//!
//! let agent = Agent::builder(client, "deepseek-v4")
//!     .system_prompt("You are a helpful assistant.")
//!     .tool(calculator_tool)
//!     .build();
//!
//! let answer = agent.run("What is 2+2?").await?;
//! ```
//!
//! # Advanced usage
//!
//! ```ignore
//! let agent = Agent::builder(client, "deepseek-v4")
//!     .memory(my_shared_memory)
//!     .tool(read_tool)
//!     .tool(shell_tool)
//!     .hook(sandbox_hook)
//!     .system_prompt("You are a coding assistant.")
//!     .max_steps(100)
//!     .with_micro_compact(5, compactable_tools)
//!     .with_macro_compact("deepseek-v4-flash")
//!     .build();
//! ```

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use memory::{Memory, SharedMemory};
use provider::{LLMClient, Message, Role};
use tools::{Tool, ToolRegistry};

use crate::agent::Agent;
use crate::context::EngineContext;
use crate::hooks::AgentHook;

/// Fluent builder for [`Agent`].
///
/// Created via [`Agent::builder`].  All optional fields have sensible
/// defaults; call the builder methods to override them.
///
/// Call [`build`](Self::build) to produce the final [`Agent`].
///
/// # Defaults
///
/// | Field | Default |
/// |-------|---------|
/// | [`memory`](Self::memory) | Auto-created empty [`Memory`] |
/// | [`tools`](Self::tool) | Empty registry |
/// | [`hooks`](Self::hook) | Empty vec |
/// | [`system_prompt`](Self::system_prompt) | `None` |
/// | [`max_steps`](Self::max_steps) | `50` |
/// | [`max_retries`](Self::max_retries) | `3` |
/// | [`streaming`](Self::streaming) | `true` |
/// | Micro-compaction | Off — call [`with_micro_compact`](Self::with_micro_compact) |
/// | Macro-compaction | Off — call [`with_macro_compact`](Self::with_macro_compact) |
pub struct AgentBuilder<C: LLMClient> {
    llm: C,
    model: String,
    memory: Option<SharedMemory>,
    tools: Vec<Arc<dyn Tool>>,
    hooks: Vec<Box<dyn AgentHook>>,
    system_prompt: Option<String>,
    max_steps: usize,
    max_retries: usize,
    streaming: bool,
    compact_tool_outputs: bool,
    keep_recent_tool_outputs: usize,
    compactable_tool_names: HashSet<String>,
    compact_model: Option<String>,
}

impl<C: LLMClient> AgentBuilder<C> {
    /// Internal constructor — use [`Agent::builder`](crate::Agent::builder) instead.
    pub(crate) fn new(llm: C, model: String) -> Self {
        Self {
            llm,
            model,
            memory: None,
            tools: Vec::new(),
            hooks: Vec::new(),
            system_prompt: None,
            max_steps: 50,
            max_retries: 3,
            streaming: true,
            compact_tool_outputs: false,
            keep_recent_tool_outputs: 5,
            compactable_tool_names: HashSet::new(),
            compact_model: None,
        }
    }

    /// Provide a shared-memory instance.
    ///
    /// If not called, a fresh [`Memory`] is created internally in
    /// [`build`](Self::build).  Use this when you need to share memory
    /// across multiple agents or pre-seed conversation history.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mem: SharedMemory = Arc::new(RwLock::new(Memory::new()));
    /// let agent = Agent::builder(client, "m").memory(mem.clone()).build();
    /// ```
    pub fn memory(mut self, memory: SharedMemory) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Register a single tool.
    ///
    /// The tool is wrapped in `Arc` automatically.  Tools are registered
    /// in insertion order; duplicate tool names are silently replaced
    /// (last wins).
    ///
    /// # Example
    ///
    /// ```ignore
    /// let agent = Agent::builder(client, "m")
    ///     .tool(CalculatorTool)
    ///     .tool(ReadTool::new(workspace))
    ///     .build();
    /// ```
    pub fn tool(mut self, tool: impl Tool + 'static) -> Self {
        self.tools.push(Arc::new(tool));
        self
    }

    /// Register multiple pre-wrapped tools at once.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let tools: Vec<Arc<dyn Tool>> = vec![
    ///     Arc::new(CalculatorTool),
    ///     Arc::new(ReadTool::new(ws)),
    /// ];
    /// let agent = Agent::builder(client, "m").tools(tools).build();
    /// ```
    pub fn tools(mut self, tools: impl IntoIterator<Item = Arc<dyn Tool>>) -> Self {
        self.tools.extend(tools);
        self
    }

    /// Register a single lifecycle hook.
    ///
    /// Hooks are called in registration order for each lifecycle event.
    /// See [`AgentHook`] for the available events.
    pub fn hook(mut self, hook: impl AgentHook + 'static) -> Self {
        self.hooks.push(Box::new(hook));
        self
    }

    /// Set the system prompt.
    ///
    /// In [`build`](Self::build) this is pushed as a `Role::System` message
    /// into memory before any user or assistant messages.  This is
    /// equivalent to manually calling:
    ///
    /// ```ignore
    /// memory.write().unwrap().push(Message::new(Role::System, prompt));
    /// ```
    ///
    /// # Example
    ///
    /// ```ignore
    /// let agent = Agent::builder(client, "m")
    ///     .system_prompt("You are a helpful coding assistant.")
    ///     .build();
    /// ```
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
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
    /// in real time via [`Agent::run_with_events`].  When disabled it uses
    /// `LLMClient::generate()` and emits a single [`AgentEvent::Token`]
    /// with the full response.
    pub fn streaming(mut self, streaming: bool) -> Self {
        self.streaming = streaming;
        self
    }

    /// Enable **micro-compaction** of tool outputs.
    ///
    /// Old tool results for the named tools are replaced with the
    /// placeholder `"[Old tool result content cleared]"`, keeping only
    /// the most recent `keep_recent` results intact.  This reduces token
    /// usage without losing the tool-call / tool-result pairing structure.
    ///
    /// # Parameters
    ///
    /// * `keep_recent` — how many of the most recent outputs to preserve
    ///   per compactable tool name.
    /// * `compactable_tools` — which tool names are eligible for
    ///   compaction (e.g. `"read"`, `"shell"`, `"grep"`).
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

    /// Enable **macro-compaction** (full LLM summarisation).
    ///
    /// When the conversation exceeds the memory character budget the
    /// agent drains old non-System messages and asks the given `model`
    /// to produce a summary, which is inserted as a new System message.
    ///
    /// Use a cheap / fast model here (e.g. `"deepseek-v4-flash"`) —
    /// the summarisation runs inline and blocks the agent loop.
    pub fn with_macro_compact(mut self, model: impl Into<String>) -> Self {
        self.compact_model = Some(model.into());
        self
    }

    /// Consume the builder and produce a fully-wired [`Agent`].
    ///
    /// # What this does
    ///
    /// 1. **Memory**: If no [`SharedMemory`] was provided via
    ///    [`memory`](Self::memory), a fresh `Memory::new()` is created
    ///    and wrapped in `Arc<RwLock<_>>`.
    /// 2. **System prompt**: If [`system_prompt`](Self::system_prompt) was
    ///    set, it is pushed into memory as a `Role::System` message.
    /// 3. **Tools**: All registered tools are collected into a
    ///    [`ToolRegistry`] and wrapped in `Arc`.
    /// 4. **Context**: An [`EngineContext`] is built via
    ///    [`EngineContextBuilder`] with all configured settings.
    /// 5. **Agent**: `Agent::new(ctx)` is returned.
    pub fn build(self) -> Agent<C> {
        // Step 1: resolve memory
        let memory = self
            .memory
            .unwrap_or_else(|| Arc::new(RwLock::new(Memory::new())));

        // Step 2: seed system prompt
        if let Some(ref prompt) = self.system_prompt {
            let mut mem = memory.write().expect("memory lock poisoned");
            mem.push(Message::new(Role::System, prompt.clone()));
        }

        // Step 3: build tool registry
        let mut registry = ToolRegistry::new();
        for tool in self.tools {
            registry.register(tool);
        }
        let tools = Arc::new(registry);

        // Step 4: delegate to EngineContextBuilder
        let mut ctx_builder = EngineContext::builder(self.llm, memory, tools, self.model)
            .hooks(self.hooks)
            .max_steps(self.max_steps)
            .max_retries(self.max_retries)
            .streaming(self.streaming);

        if self.compact_tool_outputs || !self.compactable_tool_names.is_empty() {
            ctx_builder = ctx_builder
                .with_micro_compact(self.keep_recent_tool_outputs, self.compactable_tool_names);
        }

        if let Some(model) = self.compact_model {
            ctx_builder = ctx_builder.with_macro_compact(model);
        }

        // Step 5: wrap in Agent
        Agent::new(ctx_builder.build())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream::BoxStream;
    use provider::{CompletionRequest, CompletionResponse, ProviderError, StreamChunk};

    // ── Mock LLM client ────────────────────────────────────────────────

    /// A mock [`LLMClient`] that panics if `generate` or `stream` are
    /// called — the tests below only exercise builder construction, never
    /// the agent loop.
    struct MockClient;

    impl LLMClient for MockClient {
        async fn generate(
            &self,
            _req: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unreachable!("MockClient::generate should not be called in builder tests")
        }

        async fn stream(
            &self,
            _req: CompletionRequest,
        ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
            unreachable!("MockClient::stream should not be called in builder tests")
        }
    }

    // ── Mock tool ──────────────────────────────────────────────────────

    /// A minimal [`Tool`] for testing builder registration.
    struct MockTool {
        name: &'static str,
    }

    impl MockTool {
        const fn new(name: &'static str) -> Self {
            Self { name }
        }
    }

    impl Tool for MockTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "A mock tool for testing"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn execute_stream(&self, _args: &str) -> Result<tools::ProgressStream, tools::ToolError> {
            Ok(tools::ProgressStream::done("mock output".into()))
        }
    }

    // ── Tests ──────────────────────────────────────────────────────────

    #[test]
    fn minimal_builder_produces_valid_agent() {
        let agent = Agent::builder(MockClient, "test-model").build();
        assert_eq!(agent.model(), "test-model");
        assert_eq!(agent.max_steps(), 50);
        assert!(agent.streaming());
        // Auto-created memory is empty.
        assert!(agent.memory().read().unwrap().is_empty());
    }

    #[test]
    fn system_prompt_is_pushed_to_memory() {
        let agent = Agent::builder(MockClient, "m")
            .system_prompt("You are a test bot.")
            .build();
        let mem = agent.memory().read().unwrap();
        assert_eq!(mem.message_count(), 1);
        assert_eq!(mem.messages()[0].role, Role::System);
        assert!(mem.messages()[0].content.contains("test bot"));
    }

    #[test]
    fn tool_registration_does_not_panic() {
        let agent = Agent::builder(MockClient, "m")
            .tool(MockTool::new("mock_tool"))
            .build();
        // Building succeeds.  No public registry accessor on Agent, so
        // we just verify construction doesn't panic.
        let _ = agent;
    }

    #[test]
    fn custom_max_steps_and_streaming() {
        let agent = Agent::builder(MockClient, "m")
            .max_steps(10)
            .streaming(false)
            .build();
        assert_eq!(agent.max_steps(), 10);
        assert!(!agent.streaming());
    }

    #[test]
    fn explicit_memory_is_used_instead_of_auto_created() {
        let mem: SharedMemory = Arc::new(RwLock::new(Memory::new()));
        mem.write().unwrap().push(Message::new(Role::User, "hello"));

        let agent = Agent::builder(MockClient, "m").memory(mem.clone()).build();

        // Same Arc, same memory.
        assert!(std::ptr::eq(Arc::as_ptr(agent.memory()), Arc::as_ptr(&mem),));
        // Pre-seeded message is still there.
        assert_eq!(agent.memory().read().unwrap().message_count(), 1);
    }

    #[test]
    fn system_prompt_with_explicit_memory_appends_to_it() {
        let mem: SharedMemory = Arc::new(RwLock::new(Memory::new()));
        mem.write()
            .unwrap()
            .push(Message::new(Role::User, "existing"));

        let agent = Agent::builder(MockClient, "m")
            .memory(mem.clone())
            .system_prompt("SYSTEM PROMPT")
            .build();

        let messages = agent.memory().read().unwrap();
        assert_eq!(messages.message_count(), 2);
        // System prompt is pushed after the existing message (at the end).
        assert_eq!(messages.messages()[0].role, Role::User);
        assert_eq!(messages.messages()[1].role, Role::System);
    }

    #[test]
    fn compaction_disabled_by_default() {
        // Compaction fields are not pub on AgentBuilder, so we inspect
        // the built Agent.  Since there are no getters for compaction
        // settings, we verify the build does not panic and trust the
        // default-off contract.
        let agent = Agent::builder(MockClient, "test-model").build();
        assert_eq!(agent.model(), "test-model"); // sanity check
    }

    #[test]
    fn micro_compact_enables_compaction() {
        let names: HashSet<String> = ["read", "shell"].iter().map(|s| (*s).to_string()).collect();
        let agent = Agent::builder(MockClient, "m")
            .with_micro_compact(3, names)
            .build();
        // Build succeeds — compaction is configured internally.
        let _ = agent;
    }

    #[test]
    fn macro_compact_sets_model() {
        let agent = Agent::builder(MockClient, "m")
            .with_macro_compact("deepseek-v4-flash")
            .build();
        // Build succeeds — compaction model is configured internally.
        let _ = agent;
    }
}
