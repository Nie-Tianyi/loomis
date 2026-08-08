//! Hook that seeds the initial system messages into memory on first run.
//!
//! Pushes three `Role::System` messages:
//! 1. Main system prompt (from `prompts/system.md` with dynamic tool list)
//! 2. Environment context (platform, OS, shell, cwd, date, git)
//! 3. Project rules (LOOMIS.md → AGENTS.md → CLAUDE.md)
//!
//! Seeding is idempotent via a content marker — after `/new`
//! (ClearConversation) or a process restart that loads a saved conversation,
//! the hook detects the existing messages and skips.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use engine::AgentHook;
use memory::SharedMemory;
use provider::{Message, Role};
use skills::SkillRegistry;

/// Marker prefix that identifies core system prompt messages.
///
/// Used for content-based dedup (replaces the previous `AtomicBool` guard —
/// see [`SystemPromptHook::on_run_start`]) and for selective preservation
/// across `/new` in the TUI.  Follows the same convention as `[SKILL:`,
/// `[PROFILE]`, `[TODO]`, and `[PLAN_MODE]`.
pub(crate) const SYSPROMPT_MARKER: &str = "[SYSPROMPT]";

/// Maximum bytes to load from a project-rules file before truncating.
const PROJECT_RULES_MAX_BYTES: usize = 10_000;

/// Project-level rules file candidates, in priority order.
const PROJECT_RULES_FILES: &[&str] = &["LOOMIS.md", "AGENTS.md", "CLAUDE.md"];

// ── SystemPromptHook ─────────────────────────────────────────────────────────────

/// Seeds the three initial system messages on `on_run_start`.
///
/// Deduplication is content-based: if any `[SYSPROMPT]` message is already
/// present in memory (survived `/new`, or was loaded from a saved
/// conversation after a process restart), seeding is skipped.  This is
/// robust where the previous `AtomicBool` guard was not — a fresh process
/// has `seeded == false`, so restart + load would have re-seeded duplicates.
pub struct SystemPromptHook {
    workspace_root: PathBuf,
    tool_names: Vec<String>,
    skill_registry: Arc<SkillRegistry>,
}

impl SystemPromptHook {
    pub fn new(
        workspace_root: PathBuf,
        tool_names: Vec<String>,
        skill_registry: Arc<SkillRegistry>,
    ) -> Self {
        Self {
            workspace_root,
            tool_names,
            skill_registry,
        }
    }
}

impl AgentHook for SystemPromptHook {
    fn on_run_start(&self, _session_id: &str, _user_input: &str, memory: &SharedMemory) {
        let mut mem = memory.write().expect("memory lock poisoned");

        // Content-based dedup: skip if core system prompts are already
        // present (survived /new, loaded from disk, etc.).
        let already_seeded = mem
            .messages
            .iter()
            .any(|m| m.role == Role::System && m.content.starts_with(SYSPROMPT_MARKER));
        if already_seeded {
            tracing::debug!("System prompts already seeded (marker found), skipping");
            return;
        }

        // 1. Main system prompt (dynamic tool list + skill list)
        mem.push(Message::new(
            Role::System,
            format!(
                "{SYSPROMPT_MARKER}\n\n{}",
                build_system_prompt(&self.tool_names, &self.skill_registry)
            ),
        ));

        // 2. Environment context (platform, shell, cwd, date, git)
        mem.push(Message::new(
            Role::System,
            format!(
                "{SYSPROMPT_MARKER}\n\n{}",
                build_environment_context(&self.workspace_root)
            ),
        ));

        // 3. Project rules (LOOMIS.md → AGENTS.md → CLAUDE.md)
        if let Some(rules) = try_load_project_rules(&self.workspace_root) {
            mem.push(Message::new(
                Role::System,
                format!("{SYSPROMPT_MARKER}\n\n{rules}"),
            ));
        }

        tracing::debug!(
            chars = mem.messages.iter().map(|m| m.content.len()).sum::<usize>(),
            tools = self.tool_names.len(),
            "Seeded initial system messages",
        );
    }
}

// ── System Prompt ─────────────────────────────────────────────────────────────────

/// Build the main system prompt with tool list and skill list injected dynamically.
///
/// Loaded from `prompts/system.md` at compile time via `include_str!()`.
/// `{tool_list}` and `{skill_list}` are dynamic — simple `str::replace` handles them.
fn build_system_prompt(tool_names: &[String], skill_registry: &SkillRegistry) -> String {
    let tool_list = tool_names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ");

    let skill_list = if skill_registry.is_empty() {
        "  (none available. Define skills as .md files in .loomis/skills/)".to_string()
    } else {
        skill_registry
            .list()
            .iter()
            .map(|s| format!("- `{}` — {}", s.name, s.description))
            .collect::<Vec<_>>()
            .join("\n")
    };

    include_str!("../../prompts/system.md")
        .replace("{tool_list}", &tool_list)
        .replace("{skill_list}", &skill_list)
}

// ── Environment Context ─────────────────────────────────────────────────────────

/// Build a System message with runtime environment information.
fn build_environment_context(workspace_root: &Path) -> String {
    let platform = format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH);
    let os_ver = detect_os_version();
    let shell = detect_shell();
    let cwd = workspace_root.display().to_string();
    let date = util::iso8601_now();
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
            if let Ok(out) = std::process::Command::new(cmd).args(args).output()
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

/// Report the shell backend used for command execution (not the parent
/// terminal — on Windows commands always run via `cmd`, see ShellTool).
fn detect_shell() -> String {
    if std::env::var("MSYSTEM").is_ok() || std::env::var("MINGW_PREFIX").is_ok() {
        return "Git Bash (MSYS2 / MinGW)".to_string();
    }
    #[cfg(windows)]
    {
        // Commands always execute via `cmd /C` regardless of the terminal
        // Loomis was launched from — report the real backend, not the
        // parent shell (which used to claim "PowerShell" via PSModulePath).
        "cmd".to_string()
    }
    #[cfg(not(windows))]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "unknown".to_string())
    }
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
                if !s.is_empty() { Some(s) } else { None }
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

/// Try to load project-level rules from the workspace root.
///
/// Resolution priority: `LOOMIS.md` → `AGENTS.md` → `CLAUDE.md`.
/// Only the **first found** file is returned.  If no file exists or all
/// reads fail, returns `None`.
fn try_load_project_rules(workspace_root: &Path) -> Option<String> {
    for filename in PROJECT_RULES_FILES {
        let path = workspace_root.join(filename);
        match std::fs::read_to_string(&path) {
            Ok(content) if !content.trim().is_empty() => {
                tracing::debug!(
                    file = %filename,
                    path = %path.display(),
                    bytes = content.len(),
                    "Loaded project rules",
                );
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
                return Some(format!("## Project Rules ({filename})\n\n{truncated}"));
            }
            Ok(_) => {
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "Cannot read project rules file");
                continue;
            }
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::{Arc, RwLock};

    use memory::Memory;

    use super::*;

    fn make_hook() -> SystemPromptHook {
        SystemPromptHook::new(
            PathBuf::from("."),
            vec!["read".to_string(), "write".to_string()],
            Arc::new(SkillRegistry::empty()),
        )
    }

    fn make_memory() -> SharedMemory {
        Arc::new(RwLock::new(Memory::new()))
    }

    /// All seeded messages must carry the `[SYSPROMPT]` marker so they can
    /// be detected for dedup and selective preservation.
    #[test]
    fn seeds_marked_system_prompts_on_empty_memory() {
        let hook = make_hook();
        let memory = make_memory();

        hook.on_run_start("test", "hello", &memory);

        let mem = memory.read().unwrap();
        assert!(
            !mem.messages.is_empty(),
            "expected system prompts to be seeded"
        );
        for m in mem.messages.iter() {
            assert_eq!(m.role, Role::System);
            assert!(
                m.content.starts_with(SYSPROMPT_MARKER),
                "every seeded message must start with {SYSPROMPT_MARKER}, got: {}",
                &m.content[..m.content.len().min(40)]
            );
        }
    }

    /// The core regression: after a process restart, the saved conversation
    /// is loaded with `[SYSPROMPT]` messages already present.  The hook must
    /// NOT seed again (the previous `AtomicBool` guard would have reset and
    /// pushed a duplicate set on every restart).
    #[test]
    fn skips_when_marker_already_present() {
        let hook = make_hook();
        let memory = make_memory();

        // Simulate a loaded conversation: one [SYSPROMPT] message + history.
        {
            let mut mem = memory.write().unwrap();
            mem.push(Message::new(
                Role::System,
                format!("{SYSPROMPT_MARKER}\n\nLoaded main prompt"),
            ));
            mem.push(Message::new(Role::System, "unrelated system message"));
            mem.push(Message::new(Role::User, "previous user message"));
        }
        let before: Vec<String> = memory
            .read()
            .unwrap()
            .messages
            .iter()
            .map(|m| m.content.clone())
            .collect();

        hook.on_run_start("test", "new message", &memory);

        let after: Vec<String> = memory
            .read()
            .unwrap()
            .messages
            .iter()
            .map(|m| m.content.clone())
            .collect();
        assert_eq!(
            before, after,
            "on_run_start must be a no-op when [SYSPROMPT] is already present"
        );
    }

    /// Repeated calls in the same process are no-ops too.
    #[test]
    fn repeated_calls_do_not_accumulate() {
        let hook = make_hook();
        let memory = make_memory();

        hook.on_run_start("test", "hello", &memory);
        let first_count = memory
            .read()
            .unwrap()
            .messages
            .iter()
            .filter(|m| m.content.starts_with(SYSPROMPT_MARKER))
            .count();
        // Main + environment are always seeded; project rules depend on the
        // test working directory, so only assert the minimum.
        assert!(
            first_count >= 2,
            "expected at least two core system prompts"
        );

        hook.on_run_start("test", "again", &memory);
        hook.on_run_start("test", "and again", &memory);

        let sysprompt_count = memory
            .read()
            .unwrap()
            .messages
            .iter()
            .filter(|m| m.content.starts_with(SYSPROMPT_MARKER))
            .count();
        assert_eq!(
            sysprompt_count, first_count,
            "repeated on_run_start must not add more system prompts"
        );
    }
}
