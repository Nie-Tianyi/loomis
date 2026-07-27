//! Agent assembly — wires all components together.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use deepseek::DeepSeekClient;
use engine::{Agent, EngineContext};
use hooks;
use memory::{Memory, PendingHints, PersistenceConfig, SharedMemory};
use observability::TraceStore;
use skills::{self, SkillRegistry};
use subagent::{self, SubagentConfig};
use tokio::sync::mpsc;
use tools::ToolRegistry;

use sandbox::SandboxConfig;

use crate::hooks::{
    ObservabilityHook, PersistenceHook, PlanModeHook, PlanModeState, ProfileHook, SandboxHook,
    SkillHook, SystemPromptHook, TodoListHook,
};
use crate::tools::{
    AskUserQuestionTool, CalculatorTool, EditTool, EnterPlanModeTool, ExitPlanModeTool, GlobTool,
    GrepTool, LsTool, ReadTool, ShellTool, SkillTool, TodoItem, TodoTool, WriteTool,
};
use engine::ResponseRouter;
use sandbox::audit_logger::AuditLogger;
use sandbox::resource_tracker::ResourceTracker;
use sandbox::shell_filter::ShellFilter;

// ── AgentEvent & InterventionResponse (re-exported from engine) ─────────────────

/// Re-export the engine's event type for channel construction.
pub use engine::AgentEvent;
pub use engine::InterventionResponse;

/// Product of [`build_coding_agent`] — everything needed to launch the TUI.
pub struct AgentKit {
    pub agent: Agent<DeepSeekClient>,
    pub memory: SharedMemory,
    pub tool_names: Vec<String>,
    pub model: String,
    /// Receiving half of the agent-event channel — consumed by the TUI event loop.
    pub agent_rx: mpsc::UnboundedReceiver<AgentEvent>,
    /// Clone of the sending half — for the agent handler background task.
    pub agent_tx: mpsc::UnboundedSender<AgentEvent>,
    /// Routes intervention responses to the correct requester
    /// (SandboxHook, AskUserQuestionTool, …).
    pub response_router: Arc<ResponseRouter>,
    /// Queue for user hints injected during active agent runs.
    /// Drained by the agent loop before each LLM call.
    pub pending_hints: PendingHints,
    /// Persistence config — directory layout and naming for thread storage.
    pub persistence_config: PersistenceConfig,
    /// Shared todo list state — written by [`TodoTool`], read by the TUI status bar.
    pub todos: Arc<RwLock<Vec<TodoItem>>>,
    /// Shared trace store — written by [`ObservabilityHook`], read by the TUI.
    pub trace_store: Arc<TraceStore>,
    /// Shared plan-mode toggle between TUI and [`PlanModeHook`].
    pub plan_mode: Arc<PlanModeState>,
    /// Directory where approved plans are archived (`.loomis/plan/`).
    pub plan_dir: PathBuf,
    /// Discovered skills — read-only after startup.
    pub skill_registry: Arc<SkillRegistry>,
    /// Currently active skills — written by [`SkillTool`], read by [`SkillHook`].
    pub active_skills: skills::ActiveSkills,
    /// Shell-command policy — the same instance backing [`SandboxHook`],
    /// reused by the TUI to classify user `!command` invocations
    /// (Nielsen #5: error prevention).
    pub shell_filter: sandbox::shell_filter::ShellFilter,
}

/// Seed default skills into `.loomis/skills/` if no `.md` files exist there.
///
/// Idempotent: if the directory already contains any `.md` files (user-created
/// or previously seeded), this function is a no-op. Missing directories are
/// created on demand.
///
/// Embedded content comes from `include_str!()` at compile time, so the binary
/// is self-contained — no runtime file reads.
fn seed_default_skills(workspace_root: &Path) {
    let skills_dir = workspace_root.join(".loomis").join("skills");

    // Check if any .md files already exist. If so, the user has intentionally
    // authored (or previously seeded) skills — don't touch anything.
    if skills_dir.exists() {
        let has_md = std::fs::read_dir(&skills_dir)
            .map(|rd| {
                rd.flatten()
                    .any(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            })
            .unwrap_or(false);
        if has_md {
            tracing::debug!(
                dir = %skills_dir.display(),
                "Skills already exist, skipping seed",
            );
            return;
        }
    }

    // Create the directory (idempotent).
    if let Err(e) = std::fs::create_dir_all(&skills_dir) {
        tracing::error!(
            dir = %skills_dir.display(),
            error = %e,
            "Failed to create skills directory",
        );
        return;
    }

    // Write the embedded default skill.
    let seed_path = skills_dir.join("skill-generator.md");
    let content = include_str!("../skills/skill-generator.md");

    match std::fs::write(&seed_path, content) {
        Ok(()) => tracing::info!(
            path = %seed_path.display(),
            "Seeded default skill-generator skill",
        ),
        Err(e) => tracing::error!(
            path = %seed_path.display(),
            error = %e,
            "Failed to seed default skill",
        ),
    }
}

/// Build a fully-wired coding agent with all channels, tools, and hooks.
///
/// # Assembly order
///
/// ```text
/// 1. Channels           — agent_tx/agent_rx, response_router
/// 2. Sandbox components — workspace, shell filter, resource tracker, audit
/// 3. Skills             — discover from .loomis/skills/ and ~/.loomis/skills/
/// 4. Tools              — file ops → subagent → meta → skill (see build_tool_registry)
/// 5. LLM clients        — DeepSeek, subagent, profile, compaction
/// 6. Hooks              — 10 hooks in registration order (see build_hooks)
/// 7. Engine context     — wire everything into Agent + EngineContext
/// ```
///
/// The returned [`AgentKit`] carries all shared state the TUI needs.
pub fn build_coding_agent(
    api_key: &str,
    workspace_root: &Path,
    model: &str,
    flash_model: &str,
    sandbox_config: &SandboxConfig,
) -> AgentKit {
    // ── Channels ──────────────────────────────────────────────
    let (agent_tx, agent_rx) = mpsc::unbounded_channel::<AgentEvent>();

    // ── Workspace filesystem ─────────────────────────────────
    let workspace = tools::WorkspaceFs::new(workspace_root, sandbox_config).unwrap_or_else(|e| {
        tracing::error!(
            path = %workspace_root.display(),
            error = %e,
            "Cannot create workspace",
        );
        std::process::exit(1);
    });
    let workspace = Arc::new(workspace);

    // ── Shared intervention response router ───────────────────
    // Must be created before tools — AskUserQuestionTool needs it.
    let response_router = Arc::new(ResponseRouter::new());

    // ── Pending hints queue ────────────────────────────────────
    // Decouples user hint injection from memory mutation so hints
    // never land between an assistant tool_calls message and its
    // tool results (which violates the provider API contract).
    let pending_hints = PendingHints::default();

    // ── Plan mode state ───────────────────────────────────────
    // Created before tools so EnterPlanModeTool / ExitPlanModeTool
    // can be registered and included in tool_names.
    let plan_mode = Arc::new(PlanModeState::default());
    let plan_file_path = workspace_root.join(".loomis").join("plan.md");
    let plan_dir = workspace_root.join(".loomis").join("plan");

    // ── Seed default skills ──────────────────────────────────
    // If no skills exist yet, seed the default "skill-generator"
    // skill so the user immediately has guidance on creating new skills.
    seed_default_skills(workspace_root);

    // ── Skills ────────────────────────────────────────────────
    // Discover skills from project and user directories.
    let skill_search_paths = vec![
        workspace_root.join(".loomis").join("skills"),
        dirs_fallback().join(".loomis").join("skills"),
    ];
    let skill_registry = Arc::new(SkillRegistry::discover(&skill_search_paths));
    let active_skills: skills::ActiveSkills = Arc::new(RwLock::new(HashMap::new()));

    // ── Tool registry ────────────────────────────────────────
    let mut registry = ToolRegistry::new();

    // Shared todo-list state — the TodoTool writes it, the TUI reads it.
    let todo_state = Arc::new(RwLock::new(Vec::<TodoItem>::new()));

    registry.register(Arc::new(CalculatorTool));
    registry.register(Arc::new(ReadTool::new(workspace.clone())));
    registry.register(Arc::new(EditTool::new(workspace.clone())));
    registry.register(Arc::new(WriteTool::new(workspace.clone())));
    registry.register(Arc::new(GlobTool::new(workspace.clone())));
    registry.register(Arc::new(GrepTool::new(workspace.clone())));
    registry.register(Arc::new(LsTool::new(workspace.clone())));
    registry.register(Arc::new(ShellTool::new(
        workspace_root.to_path_buf(),
        sandbox_config,
    )));

    // ── Memory ───────────────────────────────────────────────
    let memory: SharedMemory = Arc::new(std::sync::RwLock::new(Memory::new()));

    // ── Trace store (observability) ──────────────────────────
    let trace_store = Arc::new(TraceStore::new());

    // ── LLM Clients ─────────────────────────────────────────────
    let client = DeepSeekClient::new(api_key);
    let subagent_client = client.clone(); // clone before client is moved into EngineContext
    let profile_client = client.clone(); // clone for ProfileHook synthesis
    let compact_client = DeepSeekClient::new(api_key);

    // ── Subagent tool (read-only subset, no shell, no write, no task) ──
    let subagent_registry =
        subagent::filter_tools(&registry, &["read", "ls", "glob", "grep", "calculator"]);
    let subagent_registry = Arc::new(subagent_registry);

    let subagent_config = SubagentConfig {
        model: flash_model.to_string(),
        ..Default::default()
    };
    let subagent_tool = subagent::SubagentTool::new(
        subagent_client,
        subagent_config,
        subagent_registry,
        memory.clone(),
    )
    .with_trace_store(trace_store.clone());
    registry.register(Arc::new(subagent_tool));

    // AskUserQuestionTool — lets the LLM ask the user questions.
    let ask_tool = AskUserQuestionTool::new(response_router.clone());
    ask_tool.set_agent_tx(agent_tx.clone());
    registry.register(Arc::new(ask_tool));

    // TodoTool — lets the LLM manage a structured task list (plan).
    let todo_tool = TodoTool::new(todo_state.clone());
    registry.register(Arc::new(todo_tool));

    // EnterPlanModeTool — lets the LLM activate plan mode autonomously.
    let enter_plan_tool = EnterPlanModeTool::new(plan_mode.clone(), plan_file_path.clone());
    registry.register(Arc::new(enter_plan_tool));

    // ExitPlanModeTool — lets the LLM present the plan for user approval
    // and deactivate plan mode. On approval, archives the plan to .loomis/plan/.
    let exit_plan_tool = ExitPlanModeTool::new(
        plan_mode.clone(),
        plan_file_path.clone(),
        plan_dir.clone(),
        response_router.clone(),
    );
    exit_plan_tool.set_agent_tx(agent_tx.clone());
    registry.register(Arc::new(exit_plan_tool));

    // SkillTool — lets the LLM load named skill instructions.
    registry.register(Arc::new(SkillTool::new(
        skill_registry.clone(),
        active_skills.clone(),
    )));

    let tool_names: Vec<String> = registry.iter().map(|(n, _)| n.to_string()).collect();
    let registry = Arc::new(registry);

    // ── Sandbox components ────────────────────────────────────
    let shell_filter = ShellFilter::from_config(sandbox_config);
    let resource_tracker = Arc::new(ResourceTracker::new(sandbox_config));
    let audit_logger = Arc::new(AuditLogger::new(sandbox_config, workspace_root));

    // ── Hooks ─────────────────────────────────────────────────

    // ObservabilityHook — full-chain trace event collection.
    let observability_hook = ObservabilityHook::new(trace_store.clone(), memory.clone());

    // PlanModeHook — restricts tools in plan mode, injects plan-mode prompt.
    let plan_mode_hook = PlanModeHook::new(
        plan_mode.clone(),
        plan_file_path.clone(),
        workspace_root.to_path_buf(),
    );

    // SandboxHook — shell approval, resource tracking, audit logging
    let approval_hook = SandboxHook::new(
        shell_filter.clone(),
        resource_tracker,
        audit_logger,
        response_router.clone(),
    );
    approval_hook.set_agent_tx(agent_tx.clone());

    // MicroCompactHook — clears old tool output content
    let micro_compact = hooks::MicroCompactHook::new(
        hooks::DEFAULT_KEEP_RECENT_TOOL_OUTPUTS,
        hooks::DEFAULT_COMPACT_ELIGIBLE_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    );

    // MacroCompactHook — LLM summarisation when over budget.
    // Blocks the agent task via Handle::block_on (separate thread from TUI).
    let macro_compact = hooks::MacroCompactHook::new(
        flash_model.to_string(),
        hooks::DEFAULT_COMPACT_TOKEN_LIMIT,
        hooks::DEFAULT_KEEP_LAST_N,
        compact_client,
    );

    // ProfileHook — collects user behaviour signals across sessions,
    // periodically synthesises them via flash-model LLM, and injects
    // a [PROFILE] System message so the agent personalises responses.
    let profile_hook = ProfileHook::new(
        workspace_root.to_path_buf(),
        flash_model.to_string(),
        profile_client,
    );

    // SystemPromptHook — seeds the three initial system messages on first run
    // (now includes skill list in the main system prompt).
    let system_prompt_hook = SystemPromptHook::new(
        workspace_root.to_path_buf(),
        tool_names.clone(),
        skill_registry.clone(),
    );

    // TodoListHook — maintains the [TODO] System message from the shared
    // todo state.  Runs before compaction hooks so the message is present
    // in memory before any summarisation or clearing.
    let todo_list_hook = TodoListHook::new(todo_state.clone());

    // PersistenceHook — auto-saves conversation after each agent run.
    // Must match the TUI's persistence_config so both the hook and user
    // save/resume operations write to the same directory.
    let persistence_config = PersistenceConfig {
        threads_dir: ".loomis/threads".into(),
        current_thread_file: ".loomis/current".into(),
        markdown_title: "Loomis Conversation".into(),
        ..Default::default()
    };
    let persistence_hook =
        PersistenceHook::new(workspace_root.to_path_buf(), persistence_config.clone());

    let hooks: Vec<Box<dyn engine::AgentHook>> = vec![
        Box::new(system_prompt_hook), // 0. Seed system prompts on run start
        Box::new(plan_mode_hook),     // 1. Plan mode filtering + prompt injection
        Box::new(observability_hook), // 2. Full-chain trace event collection
        Box::new(persistence_hook),   // 3. Save conversation after each run
        Box::new(todo_list_hook),     // 4. Maintain [TODO] System message
        Box::new(SkillHook::new(active_skills.clone())), // 5. Maintain [SKILL: ...] System messages
        Box::new(profile_hook),       // 6. Maintain [PROFILE] System message + synthesis
        Box::new(macro_compact),      // 7. LLM summarisation
        Box::new(micro_compact),      // 8. Tool output clearing
        Box::new(approval_hook),      // 9. Security sandbox
    ];

    // ── Engine context (via builder) ─────────────────────────
    let ctx = EngineContext::builder(client, memory.clone(), registry, model.to_string())
        .hooks(hooks)
        .max_steps(50)
        .max_retries(3)
        .streaming(true)
        .pending_hints(pending_hints.clone())
        .build();

    let agent = Agent::new(ctx);

    AgentKit {
        agent,
        memory,
        tool_names,
        model: model.to_string(),
        agent_rx,
        agent_tx,
        response_router,
        pending_hints,
        persistence_config,
        todos: todo_state,
        trace_store,
        plan_mode,
        plan_dir,
        skill_registry,
        active_skills,
        shell_filter,
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────────

/// Best-effort home directory — `HOME` on Unix, `USERPROFILE` on Windows.
fn dirs_fallback() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        if let Ok(dir) = std::env::var("USERPROFILE") {
            return std::path::PathBuf::from(dir);
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(dir) = std::env::var("HOME") {
            return std::path::PathBuf::from(dir);
        }
    }
    std::path::PathBuf::from(".")
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use skills::SkillRegistry;

    #[test]
    fn test_seeds_when_no_skills_exist() {
        let tmp = tempfile::tempdir().unwrap();
        seed_default_skills(tmp.path());
        let seeded = tmp
            .path()
            .join(".loomis")
            .join("skills")
            .join("skill-generator.md");
        assert!(seeded.exists(), "seed file should be created");
        let content = std::fs::read_to_string(&seeded).unwrap();
        assert!(content.contains("name: skill-generator"));
        assert!(content.contains("YAML frontmatter"));
    }

    #[test]
    fn test_idempotent_when_skills_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().join(".loomis").join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        // Create a user skill.
        std::fs::write(
            skills_dir.join("my-skill.md"),
            "---\nname: my-skill\ndescription: User skill.\n---\nBody.",
        )
        .unwrap();

        seed_default_skills(tmp.path());

        // The default skill should NOT overwrite the user's skill.
        let user_skill = std::fs::read_to_string(skills_dir.join("my-skill.md")).unwrap();
        assert!(user_skill.contains("name: my-skill"));
        // The seed file should not exist because .md files already existed.
        assert!(
            !skills_dir.join("skill-generator.md").exists(),
            "seed file should not be created when skills already exist"
        );
    }

    #[test]
    fn test_idempotent_when_directory_missing() {
        // No .loomis/ directory at all — seed should create it.
        let tmp = tempfile::tempdir().unwrap();
        let looms_dir = tmp.path().join(".loomis");
        assert!(!looms_dir.exists(), "precondition: no .loomis dir");

        seed_default_skills(tmp.path());

        assert!(looms_dir.exists(), ".loomis should be created");
        assert!(
            looms_dir.join("skills").join("skill-generator.md").exists(),
            "seed file should be created"
        );
    }

    #[test]
    fn test_seeded_skill_discovered_by_registry() {
        let tmp = tempfile::tempdir().unwrap();
        seed_default_skills(tmp.path());
        let paths = vec![tmp.path().join(".loomis").join("skills")];
        let reg = SkillRegistry::discover(&paths);
        assert_eq!(reg.list().len(), 1);
        let skill = reg.by_name("skill-generator").unwrap();
        assert_eq!(skill.name, "skill-generator");
        assert!(!skill.content.is_empty());
        assert!(skill.content.contains("YAML frontmatter"));
    }
}
