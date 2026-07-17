//! Hook that enforces plan-mode restrictions and injects plan-mode context.
//!
//! When plan mode is active (toggled via `/plan` in the TUI), this hook:
//!
//! 1. **Injects** a `[PLAN_MODE]` System message via [`on_llm_start`] so the
//!    LLM knows it is in read-only research-and-planning mode.
//! 2. **Blocks** write/edit/shell tools via [`before_tool_call`], with one
//!    exception: `write` to the designated plan file is allowed.
//!
//! Uses the same patterns as [`TodoListHook`] (System message injection) and
//! [`SandboxHook`] (tool blocking).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use engine::AgentHook;
use memory::SharedMemory;
use provider::{Message, Role, ToolCall};

// ── PlanModeState ─────────────────────────────────────────────────────────────

/// Shared plan-mode toggle between the TUI and [`PlanModeHook`].
///
/// The TUI writes `active` when the user toggles `/plan`; the hook reads it
/// on every `before_tool_call` and `on_llm_start`.
pub struct PlanModeState {
    /// Whether plan mode is currently active.
    pub active: AtomicBool,
}

impl Default for PlanModeState {
    fn default() -> Self {
        Self {
            active: AtomicBool::new(false),
        }
    }
}

// ── PlanModeHook ─────────────────────────────────────────────────────────────

/// Marker prefix for the injected plan-mode System message.
/// Follows the same convention as [`TODO_MARKER`](crate::tools::TODO_MARKER).
const PLAN_MODE_MARKER: &str = "[PLAN_MODE]";

/// Hook that enforces plan-mode tool restrictions and injects plan-mode
/// context into the conversation.
pub struct PlanModeHook {
    /// Shared toggle between TUI and hook.
    plan_mode: Arc<PlanModeState>,
    /// Absolute canonical path to the plan file — the only writable file
    /// while in plan mode.
    plan_file_path: PathBuf,
    /// Workspace root, used to resolve relative `file_path` values from
    /// tool-call JSON arguments.
    workspace_root: PathBuf,
}

impl PlanModeHook {
    /// Create a new plan-mode hook.
    ///
    /// `plan_file_path` should be an absolute path (e.g.
    /// `workspace_root/.loomis/plan.md`). It is canonicalized here so
    /// path comparisons in `before_tool_call` are reliable.
    pub fn new(
        plan_mode: Arc<PlanModeState>,
        plan_file_path: PathBuf,
        workspace_root: PathBuf,
    ) -> Self {
        // Best-effort canonicalize at construction. If the file doesn't
        // exist yet, canonicalize the parent and join with the filename.
        let plan_file_path = Self::best_effort_canonicalize(&plan_file_path);
        Self {
            plan_mode,
            plan_file_path,
            workspace_root,
        }
    }

    /// Canonicalize a path. If it doesn't exist, canonicalize the parent and
    /// join with the filename.
    fn best_effort_canonicalize(path: &Path) -> PathBuf {
        match path.canonicalize() {
            Ok(c) => c,
            Err(_) => {
                let parent = path.parent().and_then(|p| p.canonicalize().ok());
                let filename = path.file_name().unwrap_or_default();
                match parent {
                    Some(p) => p.join(filename),
                    None => path.to_path_buf(),
                }
            }
        }
    }

    /// Check whether a tool's target file matches the plan file.
    ///
    /// Extracts `file_path` from the JSON arguments, resolves it against the
    /// workspace root, canonicalizes, and compares against [`Self::plan_file_path`].
    fn is_plan_file(&self, tool_call: &ToolCall) -> bool {
        let args: &str = &tool_call.function.arguments;
        // Extract "file_path" from the JSON arguments.
        let file_path = match serde_json::from_str::<serde_json::Value>(args)
            .ok()
            .and_then(|v| {
                v.get("file_path")
                    .and_then(|f| f.as_str().map(String::from))
            }) {
            Some(p) => p,
            None => return false,
        };

        let candidate = self.workspace_root.join(&file_path);
        let canonical = Self::best_effort_canonicalize(&candidate);
        canonical == self.plan_file_path
    }
}

impl AgentHook for PlanModeHook {
    /// Block write/edit/shell tools when plan mode is active.
    ///
    /// Allowed tools in plan mode:
    /// - `read`, `glob`, `grep`, `ls` — exploration
    /// - `calculator` — quick calculations
    /// - `ask_user_question` — clarifying questions
    /// - `todo` — track planning progress
    /// - `task` / `subagent` — delegate research (already read-only)
    /// - `write` — **only** when writing to the plan file
    ///
    /// Blocked tools in plan mode:
    /// - `edit` — blocked entirely
    /// - `shell` — blocked entirely
    /// - `write` — blocked unless targeting the plan file
    fn before_tool_call(
        &self,
        _session_id: &str,
        tool_call: &ToolCall,
    ) -> Result<(), engine::AgentError> {
        if !self.plan_mode.active.load(Ordering::SeqCst) {
            return Ok(());
        }

        let name = tool_call.function.name.as_str();

        match name {
            // Read-only exploration tools — always allowed.
            "read" | "ls" | "glob" | "grep" | "calculator" => Ok(()),

            // Interactive tools — allowed; the LLM may need to ask questions
            // or track progress during planning.
            "ask_user_question" | "todo" => Ok(()),

            // Subagent — already read-only by construction, so it's safe in plan mode.
            "task" | "subagent" => Ok(()),

            // Self-management tools — always allowed so the LLM can enter/exit
            // plan mode autonomously.
            "enter_plan_mode" | "exit_plan_mode" => Ok(()),

            // Write — allowed ONLY when targeting the plan file.
            "write" => {
                if self.is_plan_file(tool_call) {
                    Ok(())
                } else {
                    Err(engine::AgentError::ToolRejected {
                        name: name.into(),
                        reason: format!(
                            "Write is only allowed to the plan file in plan mode: {}",
                            self.plan_file_path.display()
                        ),
                    })
                }
            }

            // Edit — blocked entirely. The LLM should use `write` to the plan file instead.
            "edit" => Err(engine::AgentError::ToolRejected {
                name: name.into(),
                reason: "Edit is blocked in plan mode. Use write to update the plan file instead."
                    .into(),
            }),

            // Shell — blocked entirely. No shell commands in plan mode.
            "shell" => Err(engine::AgentError::ToolRejected {
                name: name.into(),
                reason: "Shell is blocked in plan mode (read-only).".into(),
            }),

            // Unknown / unexpected tools — conservative: block.
            _ => Err(engine::AgentError::ToolRejected {
                name: name.into(),
                reason: format!("Tool '{name}' is not allowed in plan mode."),
            }),
        }
    }

    /// Inject or remove the `[PLAN_MODE]` System message.
    ///
    /// Follows the same pattern as [`TodoListHook::on_llm_start`]:
    /// - When plan mode is **active**: ensure exactly one `[PLAN_MODE]` System
    ///   message exists at index 0.
    /// - When plan mode is **inactive**: remove any existing `[PLAN_MODE]`
    ///   System message(s).
    fn on_llm_start(&self, _session_id: &str, memory: &SharedMemory) {
        let is_active = self.plan_mode.active.load(Ordering::SeqCst);

        let mut mem = match memory.write() {
            Ok(m) => m,
            Err(_) => return,
        };

        // Remove any existing [PLAN_MODE] System message(s).
        mem.messages
            .retain(|m| !(m.role == Role::System && m.content.starts_with(PLAN_MODE_MARKER)));

        if is_active {
            // Build the plan-mode system message from the prompt template.
            let plan_file_str = self.plan_file_path.display().to_string();
            let content = format!(
                "{PLAN_MODE_MARKER}\n\n{}",
                include_str!("../../prompts/plan_mode.md")
                    .replace("{plan_file_path}", &plan_file_str)
            );
            mem.messages.insert(0, Message::new(Role::System, content));
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use memory::Memory;
    use provider::{Role, ToolCallFunction, ToolCallKind};

    fn make_state(active: bool) -> Arc<PlanModeState> {
        Arc::new(PlanModeState {
            active: AtomicBool::new(active),
        })
    }

    fn make_memory() -> SharedMemory {
        Arc::new(std::sync::RwLock::new(Memory::new()))
    }

    fn make_hook(active: bool, plan_file: &Path, workspace: &Path) -> PlanModeHook {
        PlanModeHook::new(
            make_state(active),
            plan_file.to_path_buf(),
            workspace.to_path_buf(),
        )
    }

    fn make_tool_call(name: &str, args: &str) -> ToolCall {
        ToolCall {
            index: 0,
            id: "test-id".into(),
            kind: ToolCallKind::Function,
            function: ToolCallFunction {
                name: name.into(),
                arguments: args.into(),
            },
        }
    }

    fn make_write_args(file_path: &str) -> String {
        format!(r#"{{"file_path":"{file_path}","content":"test"}}"#)
    }

    // ── Tool blocking tests ────────────────────────────────────────────────

    #[test]
    fn test_read_tool_allowed_in_plan_mode() {
        let hook = make_hook(true, Path::new("/ws/.loomis/plan.md"), Path::new("/ws"));
        let tc = make_tool_call("read", r#"{"file_path":"src/main.rs"}"#);
        assert!(hook.before_tool_call("test", &tc).is_ok());
    }

    #[test]
    fn test_write_tool_blocked_for_non_plan_file() {
        let hook = make_hook(true, Path::new("/ws/.loomis/plan.md"), Path::new("/ws"));
        let tc = make_tool_call("write", &make_write_args("src/main.rs"));
        assert!(hook.before_tool_call("test", &tc).is_err());
    }

    #[test]
    fn test_write_tool_allowed_for_plan_file() {
        // Use a temp directory so canonicalization works.
        let tmp = std::env::temp_dir().join("loomis-plan-test");
        let _ = std::fs::create_dir_all(&tmp);
        let plan_file = tmp.join(".loomis").join("plan.md");
        // Create the plan file so canonicalize works.
        if let Some(parent) = plan_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&plan_file, "");

        let hook = make_hook(true, &plan_file, &tmp);

        // Use the absolute path of the plan file as file_path.
        let abs_path = plan_file.display().to_string();
        let args = format!(
            r#"{{"file_path":"{}","content":"plan"}}"#,
            abs_path.replace('\\', "/")
        );
        let tc = make_tool_call("write", &args);
        assert!(hook.before_tool_call("test", &tc).is_ok());

        // Clean up.
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_edit_blocked_in_plan_mode() {
        let hook = make_hook(true, Path::new("/ws/.loomis/plan.md"), Path::new("/ws"));
        let tc = make_tool_call(
            "edit",
            r#"{"file_path":"src/main.rs","old_string":"x","new_string":"y"}"#,
        );
        assert!(hook.before_tool_call("test", &tc).is_err());
    }

    #[test]
    fn test_shell_blocked_in_plan_mode() {
        let hook = make_hook(true, Path::new("/ws/.loomis/plan.md"), Path::new("/ws"));
        let tc = make_tool_call("shell", r#"{"command":"ls"}"#);
        assert!(hook.before_tool_call("test", &tc).is_err());
    }

    #[test]
    fn test_all_tools_allowed_when_not_in_plan_mode() {
        let hook = make_hook(false, Path::new("/ws/.loomis/plan.md"), Path::new("/ws"));
        for (name, args) in [
            ("read", r#"{"file_path":"x"}"#),
            ("write", &make_write_args("src/main.rs")),
            (
                "edit",
                r#"{"file_path":"x","old_string":"a","new_string":"b"}"#,
            ),
            ("shell", r#"{"command":"ls"}"#),
            ("glob", r#"{"pattern":"*"}"#),
            ("grep", r#"{"pattern":"foo"}"#),
            ("ls", r#"{"path":"."}"#),
            ("calculator", r#"{"expression":"1+1"}"#),
            ("ask_user_question", r#"{"questions":[]}"#),
            ("todo", r#"{"todos":[]}"#),
            ("task", r#"{"description":"x","prompt":"y"}"#),
            ("enter_plan_mode", "{}"),
            ("exit_plan_mode", "{}"),
        ] {
            let tc = make_tool_call(name, args);
            assert!(
                hook.before_tool_call("test", &tc).is_ok(),
                "tool '{name}' should be allowed when not in plan mode"
            );
        }
    }

    #[test]
    fn test_subagent_allowed_in_plan_mode() {
        let hook = make_hook(true, Path::new("/ws/.loomis/plan.md"), Path::new("/ws"));
        let tc = make_tool_call("task", r#"{"description":"research","prompt":"explore"}"#);
        assert!(hook.before_tool_call("test", &tc).is_ok());
    }

    // ── System message injection tests ─────────────────────────────────────

    #[test]
    fn test_injects_plan_mode_system_message_when_active() {
        let plan_file = Path::new("/ws/.loomis/plan.md");
        let hook = make_hook(true, plan_file, Path::new("/ws"));
        let memory = make_memory();

        // Pre-seed with a normal System message.
        {
            let mut mem = memory.write().unwrap();
            mem.push(Message::new(Role::System, "Normal system prompt"));
        }

        hook.on_llm_start("test", &memory);

        let mem = memory.read().unwrap();
        let plan_msg = mem
            .messages
            .iter()
            .find(|m| m.role == Role::System && m.content.starts_with(PLAN_MODE_MARKER));
        assert!(plan_msg.is_some(), "expected [PLAN_MODE] System message");
        let content = &plan_msg.unwrap().content;
        assert!(
            content.contains("PLAN MODE"),
            "should contain PLAN MODE heading"
        );
        assert!(
            content.contains(plan_file.display().to_string().as_str()),
            "should contain plan file path"
        );
    }

    #[test]
    fn test_removes_plan_mode_system_message_when_inactive() {
        let hook = make_hook(false, Path::new("/ws/.loomis/plan.md"), Path::new("/ws"));
        let memory = make_memory();

        // Pre-seed with a [PLAN_MODE] message.
        {
            let mut mem = memory.write().unwrap();
            mem.push(Message::new(
                Role::System,
                format!("{PLAN_MODE_MARKER}\n\nPlan mode instructions."),
            ));
            mem.push(Message::new(Role::System, "Normal system prompt"));
        }

        hook.on_llm_start("test", &memory);

        let mem = memory.read().unwrap();
        let plan_count = mem
            .messages
            .iter()
            .filter(|m| m.role == Role::System && m.content.starts_with(PLAN_MODE_MARKER))
            .count();
        assert_eq!(plan_count, 0, "[PLAN_MODE] message should be removed");
        assert!(
            mem.messages
                .iter()
                .any(|m| m.role == Role::System && m.content == "Normal system prompt"),
            "non-plan System message should be preserved"
        );
    }

    #[test]
    fn test_replaces_existing_plan_mode_message() {
        let plan_file = Path::new("/ws/.loomis/plan.md");
        let hook = make_hook(true, plan_file, Path::new("/ws"));
        let memory = make_memory();

        // Pre-seed with an old [PLAN_MODE] message.
        {
            let mut mem = memory.write().unwrap();
            mem.push(Message::new(
                Role::System,
                format!("{PLAN_MODE_MARKER}\n\nOld plan mode content."),
            ));
        }

        hook.on_llm_start("test", &memory);

        let mem = memory.read().unwrap();
        let plan_count = mem
            .messages
            .iter()
            .filter(|m| m.role == Role::System && m.content.starts_with(PLAN_MODE_MARKER))
            .count();
        assert_eq!(
            plan_count, 1,
            "should still have exactly one [PLAN_MODE] message"
        );
        let plan_msg = mem
            .messages
            .iter()
            .find(|m| m.role == Role::System && m.content.starts_with(PLAN_MODE_MARKER))
            .unwrap();
        assert!(
            !plan_msg.content.contains("Old plan mode"),
            "should NOT contain old content, got: {}",
            plan_msg.content
        );
    }

    #[test]
    fn test_todo_and_ask_user_question_allowed_in_plan_mode() {
        let hook = make_hook(true, Path::new("/ws/.loomis/plan.md"), Path::new("/ws"));
        assert!(
            hook.before_tool_call("test", &make_tool_call("todo", r#"{"todos":[]}"#))
                .is_ok()
        );
        assert!(
            hook.before_tool_call(
                "test",
                &make_tool_call("ask_user_question", r#"{"questions":[]}"#)
            )
            .is_ok()
        );
    }

    #[test]
    fn test_enter_and_exit_plan_mode_allowed_when_active() {
        let hook = make_hook(true, Path::new("/ws/.loomis/plan.md"), Path::new("/ws"));
        assert!(
            hook.before_tool_call("test", &make_tool_call("enter_plan_mode", "{}"))
                .is_ok()
        );
        assert!(
            hook.before_tool_call("test", &make_tool_call("exit_plan_mode", "{}"))
                .is_ok()
        );
    }

    #[test]
    fn test_unknown_tool_blocked_in_plan_mode() {
        let hook = make_hook(true, Path::new("/ws/.loomis/plan.md"), Path::new("/ws"));
        assert!(
            hook.before_tool_call("test", &make_tool_call("unknown_tool", r#"{}"#))
                .is_err()
        );
    }
}
