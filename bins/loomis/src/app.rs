//! Agent assembly — wires all components together.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use deepseek::DeepSeekClient;
use engine::{Agent, EngineContext};
use memory::{Memory, SharedMemory};
use provider::{Message, Role};
use tools::ToolRegistry;

use crate::hooks::{CliLoggerHook, DangerousCommandApprovalHook};
use crate::tools::{CalculatorTool, GlobTool, GrepTool, LsTool, ReadTool, ShellTool, WriteTool};

/// System prompt used as the initial seed for every conversation.
pub const SYSTEM_PROMPT: &str = "\
You are Loomis, a helpful, accurate coding assistant. You have tools for file operations \
(read, write, edit, glob, grep, ls) and calculations.

## Core rules — follow strictly

1. **Ground everything in tools.** Before making ANY claim about file paths, \
code contents, directory structure, or the codebase: verify with the \
appropriate tool. Never guess.

2. **Express uncertainty.** If you don't know something or can't verify it, \
say so.

3. **Quote, don't summarise from memory.** When referencing code, always read \
the file first and quote the actual content.

4. **Verify before editing.** Before writing or editing a file, read it first.

5. **Be concise and accurate.** Short, factual responses are better than long, \
speculative ones.
";

/// Build a fully-wired coding agent.
///
/// Returns the agent and the shared memory handle (for TUI access).
pub fn build_coding_agent(
    api_key: &str,
    workspace_root: &Path,
    model: &str,
) -> (Agent, SharedMemory, Vec<String>, String) {
    // ── Workspace filesystem ─────────────────────────────────
    let workspace = tools::WorkspaceFs::new(workspace_root).unwrap_or_else(|e| {
        eprintln!(
            "ERROR: Cannot create workspace at {}: {e}",
            workspace_root.display()
        );
        std::process::exit(1);
    });
    let workspace = Arc::new(workspace);

    // ── Tool registry ────────────────────────────────────────
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(CalculatorTool));
    registry.register(Arc::new(ReadTool::new(workspace.clone())));
    registry.register(Arc::new(WriteTool::new(workspace.clone())));
    registry.register(Arc::new(GlobTool::new(workspace.clone())));
    registry.register(Arc::new(GrepTool::new(workspace.clone())));
    registry.register(Arc::new(LsTool::new(workspace.clone())));
    registry.register(Arc::new(ShellTool::new(
        workspace_root.to_path_buf(),
        Duration::from_secs(30),
    )));

    let tool_names: Vec<String> = registry.iter().map(|(n, _)| n.to_string()).collect();
    let registry = Arc::new(registry);

    // ── Memory ───────────────────────────────────────────────
    let memory: SharedMemory = Arc::new(std::sync::RwLock::new(Memory::new()));

    // ── LLM Client ───────────────────────────────────────────
    let client = DeepSeekClient::new(api_key);

    // ── Hooks ─────────────────────────────────────────────────
    let hooks: Vec<Box<dyn engine::AgentHook>> = vec![
        Box::new(CliLoggerHook),
        Box::new(DangerousCommandApprovalHook),
    ];

    // ── Engine context ────────────────────────────────────────
    let ctx = EngineContext {
        llm: Box::new(client),
        memory: memory.clone(),
        tools: registry,
        hooks,
        model: model.to_string(),
        max_steps: 50,
        max_retries: 3,
        streaming: true,
    };

    let agent = Agent::new(ctx);

    // ── Seed system prompt ────────────────────────────────────
    {
        let mut mem = memory.write().unwrap();
        mem.push(Message::new(Role::System, SYSTEM_PROMPT));
    }

    (agent, memory, tool_names, model.to_string())
}
