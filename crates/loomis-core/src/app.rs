//! Agent assembly — wires all components together.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use agent_oxide::deepseek::DeepSeekClient;
use agent_oxide::engine::{Agent, EngineContext};
use agent_oxide::memory::{Memory, PendingHints, SharedMemory};
use agent_oxide::observability::TraceStore;
use agent_oxide::persistence::PersistenceConfig;
use agent_oxide::skills::SkillRegistry;
use agent_oxide::subagent::SubagentConfig;
use agent_oxide::tools::ToolRegistry;
use tokio::sync::mpsc;

use agent_oxide::sandbox::SandboxConfig;

use crate::hooks::{
    ObservabilityHook, PlanModeHook, PlanModeState, ProfileHook, SandboxHook, SkillHook,
    SystemPromptHook, TodoListHook,
};
use crate::runtime::BuildError;
use crate::tools::{
    AskUserQuestionTool, CalculatorTool, EditTool, EnterPlanModeTool, ExitPlanModeTool, GlobTool,
    GrepTool, LsTool, ReadTool, ShellTool, SkillTool, TodoItem, TodoTool, WriteTool,
};
use agent_oxide::engine::ResponseRouter;
use agent_oxide::sandbox::audit_logger::AuditLogger;
use agent_oxide::sandbox::resource_tracker::ResourceTracker;
use agent_oxide::sandbox::shell_filter::ShellFilter;

use agent_oxide::engine::AgentEvent;

/// Product of [`assemble`] — everything the runtime driver needs.
pub(crate) struct AgentKit {
    pub agent: Agent<DeepSeekClient>,
    pub memory: SharedMemory,
    pub tool_names: Vec<String>,
    pub model: String,
    /// Receiving half of the agent-event channel — consumed by the frontend event loop.
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
    /// Shared todo list state — written by [`TodoTool`], read by frontend status bars.
    pub todos: Arc<RwLock<Vec<TodoItem>>>,
    /// Shared trace store — written by [`ObservabilityHook`], read by frontends.
    pub trace_store: Arc<TraceStore>,
    /// Shared plan-mode toggle between frontend and [`PlanModeHook`].
    pub plan_mode: Arc<PlanModeState>,
    /// Directory where approved plans are archived (`.loomis/plan/`).
    pub plan_dir: PathBuf,
    /// Discovered skills — read-only after startup.
    pub skill_registry: Arc<SkillRegistry>,
    /// Currently active skills — written by [`SkillTool`], read by [`SkillHook`].
    pub active_skills: agent_oxide::skills::ActiveSkills,
    /// Shell-command policy — the same instance backing [`SandboxHook`],
    /// exposed via [`Runtime::classify_shell`](crate::runtime::Runtime::classify_shell)
    /// so frontends can classify user `!command` invocations
    /// (Nielsen #5: error prevention).
    pub shell_filter: agent_oxide::sandbox::shell_filter::ShellFilter,
    /// Shell execution chain (env sanitisation, tree watchdog, bounded
    /// capture) — used by the driver for user `!command` runs. Policy-free;
    /// the driver classifies via [`shell_filter`](Self::shell_filter) first.
    pub shell_runner: agent_oxide::sandbox::ShellRunner,
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

/// The `/init` prompt — project-rules initialisation instructions.
///
/// Embedded via `include_str!` at compile time. Owned by the core so
/// frontends never need to read the core resource directory.
pub(crate) fn init_prompt(extra: Option<&str>) -> String {
    let init_prompt = include_str!("../prompts/init.md");
    match extra {
        Some(rest) => {
            format!("{init_prompt}\n\n### Additional instruction from the user\n\n{rest}")
        }
        None => init_prompt.to_string(),
    }
}

/// Assemble a fully-wired agent with all channels, tools, and hooks.
///
/// Crate-internal — frontends obtain the result via [`Runtime::build`]
/// (crate::runtime::Runtime::build).
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
/// The returned [`AgentKit`] carries all shared state the runtime driver needs.
pub(crate) fn assemble(
    api_key: &str,
    workspace_root: &Path,
    model: &str,
    flash_model: &str,
    sandbox_config: &SandboxConfig,
    persistence_config: &PersistenceConfig,
) -> Result<AgentKit, BuildError> {
    // Own the config here so `clone()` below dereferences to the value
    // type (a `&T.clone()` would clone the reference).
    let persistence_config = persistence_config.clone();

    // ── Channels ──────────────────────────────────────────────
    let (agent_tx, agent_rx) = mpsc::unbounded_channel::<AgentEvent>();

    // ── Workspace filesystem ─────────────────────────────────
    let workspace =
        agent_oxide::sandbox::WorkspaceFs::new(workspace_root, &sandbox_config.filesystem)
            .map_err(|e| {
                tracing::error!(
                    path = %workspace_root.display(),
                    error = %e,
                    "Cannot create workspace",
                );
                BuildError::Workspace(e)
            })?;
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
    let active_skills: agent_oxide::skills::ActiveSkills = Arc::new(RwLock::new(HashMap::new()));

    // ── Tool registry ────────────────────────────────────────
    let mut registry = ToolRegistry::new();

    // Shared todo-list state — the TodoTool writes it, frontends read it.
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
    let title_client = client.clone(); // clone for PersistenceHook title generation
    let compact_client = DeepSeekClient::new(api_key);

    // ── Subagent tool (read-only subset, no shell, no write, no task) ──
    let subagent_registry = agent_oxide::subagent::filter_tools(
        &registry,
        &["read", "ls", "glob", "grep", "calculator"],
    );
    let subagent_registry = Arc::new(subagent_registry);

    let subagent_config = SubagentConfig {
        model: flash_model.to_string(),
        ..Default::default()
    };
    let subagent_tool = agent_oxide::subagent::SubagentTool::new(
        subagent_client,
        subagent_config,
        subagent_registry,
        memory.clone(),
    )
    .with_trace_store(trace_store.clone());
    registry.register(Arc::new(subagent_tool));

    // AskUserQuestionTool — lets the LLM ask the user questions.
    let ask_tool = AskUserQuestionTool::new(response_router.clone(), agent_tx.clone());
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
        agent_tx.clone(),
    );
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
    // Execution chain for user `!command` runs — same config as the
    // ShellTool's own runner (both built from sandbox_config).
    let shell_runner = agent_oxide::sandbox::ShellRunner::new(
        workspace_root.to_path_buf(),
        sandbox_config.shell.clone(),
    );
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
    let micro_compact = agent_oxide::hooks::MicroCompactHook::new(
        agent_oxide::hooks::DEFAULT_KEEP_RECENT_TOOL_OUTPUTS,
        agent_oxide::hooks::DEFAULT_COMPACT_ELIGIBLE_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    );

    // MacroCompactHook — LLM summarisation when over budget.
    // Blocks the agent task via agent_oxide::engine::block_on (separate thread from the frontend).
    let macro_compact = agent_oxide::hooks::MacroCompactHook::new(
        flash_model.to_string(),
        agent_oxide::hooks::DEFAULT_COMPACT_TOKEN_LIMIT,
        agent_oxide::hooks::DEFAULT_KEEP_LAST_N,
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

    // PersistenceHook — auto-saves conversation after each agent run,
    // and titles it via the flash model on the first query.  Shares the
    // caller-provided persistence config so the hook and user save/resume
    // operations write to the same directory.
    let persistence_hook = agent_oxide::persistence::PersistenceHook::new(
        workspace_root.to_path_buf(),
        persistence_config.clone(),
        title_client,
        flash_model.to_string(),
    );

    let hooks: Vec<Box<dyn agent_oxide::engine::AgentHook>> = vec![
        Box::new(system_prompt_hook), // 0. Seed system prompts on run start
        Box::new(observability_hook), // 1. Full-chain trace event collection
        Box::new(persistence_hook),   // 2. Save conversation after each run
        Box::new(SkillHook::new(active_skills.clone())), // 3. Maintain [SKILL: ...] System messages
        Box::new(plan_mode_hook),     // 4. Plan mode filtering + prompt injection — registered
        //    after Skill so toggling /plan only invalidates the
        //    cache prefix past the stable system prompt + skills
        Box::new(profile_hook), // 5. Maintain [PROFILE] System message + synthesis
        Box::new(micro_compact), // 6. Tool output clearing — runs BEFORE the macro
        //    summariser so the compaction input is already
        //    placeholder-sized, not raw tool dumps (cheaper, cleaner)
        Box::new(macro_compact), // 7. LLM summarisation — registered before TodoListHook so
        //    [COMPACT_SUMMARY] sits before [TODO] in the System block:
        //    the summary only changes when compaction fires (rare),
        //    while [TODO] changes on every plan update
        Box::new(todo_list_hook), // 8. Maintain [TODO] System message — the most volatile
        //    injector; registered last so it lands at the very tail
        //    of the System block (after [COMPACT_SUMMARY], hugging
        //    the history), so a plan update only invalidates the
        //    cache prefix past the history boundary, never the
        //    compaction summary (see insert_before_history)
        Box::new(approval_hook), // 9. Security sandbox
    ];

    tracing::info!(
        %model,
        tools = tool_names.len(),
        hooks = hooks.len(),
        skills = skill_registry.list().len(),
        "Coding agent assembled",
    );

    // ── Engine context (via builder) ─────────────────────────
    let ctx = EngineContext::builder(client, memory.clone(), registry, model.to_string())
        .hooks(hooks)
        .max_steps(999)
        .max_retries(3)
        .streaming(true)
        .pending_hints(pending_hints.clone())
        .build();

    let agent = Agent::new(ctx);

    Ok(AgentKit {
        agent,
        memory,
        tool_names,
        model: model.to_string(),
        agent_rx,
        agent_tx,
        response_router,
        pending_hints,
        persistence_config: persistence_config.clone(),
        todos: todo_state,
        trace_store,
        plan_mode,
        plan_dir,
        skill_registry,
        active_skills,
        shell_filter,
        shell_runner,
    })
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
    use agent_oxide::skills::SkillRegistry;

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
