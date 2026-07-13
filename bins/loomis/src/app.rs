//! Agent assembly — wires all components together.

use std::path::Path;
use std::sync::Arc;

use deepseek::DeepSeekClient;
use engine::{Agent, EngineContext};
use hooks;
use memory::{Memory, PendingHints, PersistenceConfig, SharedMemory};
use provider::{Message, Role};
use subagent::{self, SubagentConfig};
use tokio::sync::mpsc;
use tools::ToolRegistry;

use tools::SandboxConfig;

use crate::hooks::SandboxHook;
use crate::sandbox::audit_logger::AuditLogger;
use crate::sandbox::resource_tracker::ResourceTracker;
use crate::sandbox::shell_filter::ShellFilter;
use crate::tools::{
    AskUserQuestionTool, CalculatorTool, EditTool, GlobTool, GrepTool, LsTool, ReadTool, ShellTool,
    WriteTool,
};
use engine::ResponseRouter;

/// Build the main system prompt with tool list injected dynamically.
///
/// Replaces the old flat `SYSTEM_PROMPT` const.  The prompt is organised into
/// five numbered sections matching the Claude Code system-prompt structure:
///
/// 1. Identity & Capabilities
/// 2. Tool Usage Norms
/// 3. Safety Boundaries
/// 4. Behavior Norms
/// 5. Memory & Persistence
pub fn build_system_prompt(tool_names: &[String]) -> String {
    let tool_list = tool_names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ");

    // Loaded from prompts/system.md at compile time via include_str!().
    // Only {tool_list} is dynamic — a simple str::replace handles it.
    include_str!("../prompts/system.md").replace("{tool_list}", &tool_list)
}

// ── Environment Context ─────────────────────────────────────────────────────────

/// Build a System message with runtime environment information.
///
/// Injected as a separate `Role::System` message so it can be updated or
/// removed independently (e.g. on `/new`).
pub fn build_environment_context(workspace_root: &Path) -> String {
    let platform = format!(
        "{} ({})",
        std::env::consts::OS,
        std::env::consts::ARCH
    );

    let os_ver = detect_os_version();

    let shell = detect_shell();

    let cwd = workspace_root.display().to_string();

    let date = memory::iso8601_now();

    let git_info = detect_git_info(workspace_root);

    let mut block = format!(
        "\
## Environment

- Platform: {platform}
- OS version: {os_ver}
- Shell: {shell}
- Workspace: {cwd}
- Date: {date}"
    );

    if let Some(git) = git_info {
        block.push_str(&format!("\n- Git: {git}"));
    }

    block
}

/// Best-effort OS version string.
fn detect_os_version() -> String {
    if cfg!(windows) {
        // PowerShell first — Unicode-native, no code-page issues on
        // non-English Windows.  cmd /C ver is the fallback because it
        // emits OEM-codepage bytes that turn into mojibake with UTF-8.
        for (cmd, args) in [
            (
                "powershell",
                &[
                    "-NoProfile",
                    "-Command",
                    "[System.Environment]::OSVersion.VersionString",
                ] as &[_],
            ),
            ("cmd", &["/C", "ver"] as &[_]),
        ] {
            if let Ok(out) = std::process::Command::new(cmd)
                .args(args)
                .output()
                && out.status.success()
            {
                let s = String::from_utf8_lossy(&out.stdout);
                let s = s.trim().to_string();
                if !s.is_empty() {
                    return s;
                }
            }
        }
        std::env::consts::OS.to_string()
    } else {
        match std::process::Command::new("uname").args(["-srm"]).output() {
            Ok(out) => {
                let s = String::from_utf8_lossy(&out.stdout);
                s.trim().to_string()
            }
            Err(_) => format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        }
    }
}

/// Detect which shell the user is running under.
fn detect_shell() -> String {
    // Environment-variable check (cheap).
    if std::env::var("MSYSTEM").is_ok() || std::env::var("MINGW_PREFIX").is_ok() {
        return "Git Bash (MSYS2 / MinGW)".to_string();
    }
    #[cfg(windows)]
    {
        if std::env::var("PSModulePath").is_ok() {
            return "PowerShell".to_string();
        }
        if let Ok(comspec) = std::env::var("ComSpec") {
            return comspec;
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(shell) = std::env::var("SHELL") {
            return shell;
        }
    }
    "unknown".to_string()
}

/// Best-effort git branch and dirty-status string.
///
/// Returns `None` when git is not installed or we're not inside a repo.
fn detect_git_info(workspace_root: &Path) -> Option<String> {
    let branch = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(workspace_root)
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout);
                let s = s.trim().to_string();
                if !s.is_empty() {
                    Some(s)
                } else {
                    None
                }
            } else {
                None
            }
        })?;

    let dirty = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace_root)
        .output()
        .ok()
        .map(|out| !String::from_utf8_lossy(&out.stdout).trim().is_empty())
        .unwrap_or(false);

    let status = if dirty { "dirty" } else { "clean" };
    Some(format!("branch `{branch}`, {status}"))
}

// ── Project Rules ───────────────────────────────────────────────────────────────

/// Project-level rules file candidates, in priority order.
const PROJECT_RULES_FILES: &[&str] = &["LOOMIS.md", "AGENTS.md", "CLAUDE.md"];

/// Maximum bytes to load from a project-rules file before truncating.
const PROJECT_RULES_MAX_BYTES: usize = 10_000;

/// Try to load project-level rules from the workspace root.
///
/// Resolution priority: `LOOMIS.md` → `AGENTS.md` → `CLAUDE.md`.
/// Only the **first found** file is returned.  If no file exists or all
/// reads fail, returns `None`.
pub fn try_load_project_rules(workspace_root: &Path) -> Option<String> {
    for filename in PROJECT_RULES_FILES {
        let path = workspace_root.join(filename);
        match std::fs::read_to_string(&path) {
            Ok(content) if !content.trim().is_empty() => {
                let truncated = if content.len() > PROJECT_RULES_MAX_BYTES {
                    let boundary = content
                        .char_indices()
                        .take(PROJECT_RULES_MAX_BYTES)
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(PROJECT_RULES_MAX_BYTES);
                    format!(
                        "{}…\n\n[Truncated from {} bytes — original file is {} bytes]",
                        &content[..boundary],
                        PROJECT_RULES_MAX_BYTES,
                        content.len()
                    )
                } else {
                    content
                };
                return Some(format!(
                    "## Project Rules ({filename})\n\n{truncated}"
                ));
            }
            Ok(_) => {
                // File is empty — skip to next candidate.
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Expected — try the next candidate.
                continue;
            }
            Err(e) => {
                // Permission denied, etc. — log and skip.
                eprintln!(
                    "WARNING: Cannot read {}: {e}",
                    path.display()
                );
                continue;
            }
        }
    }
    None
}

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
}

/// Build a fully-wired coding agent with all channels and hooks.
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
        eprintln!(
            "ERROR: Cannot create workspace at {}: {e}",
            workspace_root.display()
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

    // ── Tool registry ────────────────────────────────────────
    let mut registry = ToolRegistry::new();
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

    // ── LLM Clients ─────────────────────────────────────────────
    let client = DeepSeekClient::new(api_key);
    let subagent_client = client.clone(); // clone before client is moved into EngineContext
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
    );
    registry.register(Arc::new(subagent_tool));

    // AskUserQuestionTool — lets the LLM ask the user questions.
    let ask_tool = AskUserQuestionTool::new(response_router.clone());
    ask_tool.set_agent_tx(agent_tx.clone());
    registry.register(Arc::new(ask_tool));

    let tool_names: Vec<String> = registry.iter().map(|(n, _)| n.to_string()).collect();
    let registry = Arc::new(registry);

    // ── Sandbox components ────────────────────────────────────
    let shell_filter = ShellFilter::from_config(sandbox_config);
    let resource_tracker = Arc::new(ResourceTracker::new(sandbox_config));
    let audit_logger = Arc::new(AuditLogger::new(sandbox_config, workspace_root));

    // ── Hooks ─────────────────────────────────────────────────
    // SandboxHook — shell approval, resource tracking, audit logging
    let approval_hook = SandboxHook::new(
        shell_filter,
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
        hooks::DEFAULT_COMPACT_CHAR_LIMIT,
        hooks::DEFAULT_KEEP_LAST_N,
        compact_client,
    );

    let hooks: Vec<Box<dyn engine::AgentHook>> = vec![
        Box::new(macro_compact),
        Box::new(micro_compact),
        Box::new(approval_hook),
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

    // ── Seed system messages ──────────────────────────────────
    {
        let mut mem = memory.write().expect("memory lock poisoned");
        // 1. Main system prompt (5 sections, dynamic tool list)
        mem.push(Message::new(
            Role::System,
            build_system_prompt(&tool_names),
        ));
        // 2. Environment context (platform, shell, cwd, date, git)
        mem.push(Message::new(
            Role::System,
            build_environment_context(workspace_root),
        ));
        // 3. Project rules (LOOMIS.md → AGENTS.md → CLAUDE.md)
        if let Some(rules) = try_load_project_rules(workspace_root) {
            mem.push(Message::new(Role::System, rules));
        }
    }

    AgentKit {
        agent,
        memory,
        tool_names,
        model: model.to_string(),
        agent_rx,
        agent_tx,
        response_router,
        pending_hints,
        persistence_config: PersistenceConfig::default(),
    }
}
