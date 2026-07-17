//! [`EnterPlanModeTool`] — lets the LLM enter plan mode autonomously.
//!
//! When the tool is called, it activates plan mode (sets the shared
//! [`PlanModeState::active`] flag) and ensures the plan file directory
//! exists.  The LLM can call this proactively when it judges a task is
//! complex enough to warrant read-only research and planning first.
//!
//! Users can also enter plan mode manually via the `/plan` slash command.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use schemars::JsonSchema;
use serde::Deserialize;
use tools::{ProgressStream, ToolError, tool};

use crate::hooks::PlanModeState;

// ── Args ────────────────────────────────────────────────────────────────────

/// Empty args — the tool takes no parameters.
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnterPlanModeArgs {}

// ── Tool ────────────────────────────────────────────────────────────────────

/// Activates plan mode and creates the plan file directory.
///
/// Plan mode restricts the agent to read-only operations.  The agent can
/// explore, search, and write plans to the designated plan file, but cannot
/// edit source code or run shell commands.
///
/// # Response
///
/// - Already in plan mode → confirms current state
/// - Successful activation → plan file path and allowed tools summary
#[tool(
    name = "enter_plan_mode",
    description = "Enter plan mode — a read-only mode for research and planning. \
         In plan mode, you can only read files, search code, and write to the \
         plan file (.loomis/plan.md). You cannot edit source files or run shell \
         commands.\n\n\
         When to use:\n\
         - The user asks for a complex multi-step task that requires research first\n\
         - You need to explore the codebase before making changes\n\
         - The task has architectural implications and the user should review a plan\n\
         - You are unsure about the best approach and want to plan before coding\n\n\
         When NOT to use:\n\
         - The user explicitly tells you what to do (no planning needed)\n\
         - The task is a simple, single-file change\n\
         - The user asks you to \"just do it\" or \"go ahead\"\n\
         - Trivial tasks like fixing a typo or adding a comment",
    args = EnterPlanModeArgs
)]
pub struct EnterPlanModeTool {
    /// Shared plan-mode toggle between tool, hook, and TUI.
    plan_mode: Arc<PlanModeState>,
    /// Absolute path to the plan file (only writable file in plan mode).
    plan_file_path: PathBuf,
}

impl EnterPlanModeTool {
    /// Creates a new tool that shares the given plan-mode state.
    pub fn new(plan_mode: Arc<PlanModeState>, plan_file_path: PathBuf) -> Self {
        Self {
            plan_mode,
            plan_file_path,
        }
    }

    fn execute_stream(&self, _args: EnterPlanModeArgs) -> Result<ProgressStream, ToolError> {
        if self.plan_mode.active.load(Ordering::SeqCst) {
            return Ok(ProgressStream::done(format!(
                "Already in plan mode. Plan file: {}",
                self.plan_file_path.display()
            )));
        }

        // Ensure the .loomis/ directory exists.
        if let Some(parent) = self.plan_file_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ToolError::Execution(format!("Failed to create plan file directory: {e}"))
            })?;
        }

        self.plan_mode.active.store(true, Ordering::SeqCst);

        Ok(ProgressStream::done(format!(
            "Plan mode activated. You are now in read-only research and planning mode.\n\
             Plan file: {}\n\
             Allowed tools: read, glob, grep, ls, calculator, ask_user_question, \
             todo, task/subagent, write (plan file only).\n\
             Blocked tools: edit, shell.\n\
             When your plan is ready, call exit_plan_mode to present it for approval.",
            self.plan_file_path.display()
        )))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tools::Tool;

    fn make_plan_file() -> PathBuf {
        let tmp = std::env::temp_dir().join("loomis-enter-plan-test");
        let _ = std::fs::create_dir_all(&tmp);
        tmp.join(".loomis").join("plan.md")
    }

    #[test]
    fn test_name() {
        let plan_file = make_plan_file();
        let tool = EnterPlanModeTool::new(Arc::new(PlanModeState::default()), plan_file);
        assert_eq!(tool.name(), "enter_plan_mode");
    }

    #[test]
    fn test_description() {
        let plan_file = make_plan_file();
        let tool = EnterPlanModeTool::new(Arc::new(PlanModeState::default()), plan_file);
        assert!(tool.description().contains("plan mode"));
    }

    #[test]
    fn test_parameters_schema() {
        let plan_file = make_plan_file();
        let tool = EnterPlanModeTool::new(Arc::new(PlanModeState::default()), plan_file);
        let params = tool.parameter_schema();
        assert_eq!(params["type"], "object");
        assert_eq!(params["additionalProperties"], false);
    }

    #[test]
    fn test_activates_plan_mode() {
        let plan_file = make_plan_file();
        let state = Arc::new(PlanModeState::default());
        let tool = EnterPlanModeTool::new(state.clone(), plan_file);

        assert!(!state.active.load(Ordering::SeqCst));
        let mut stream = Tool::execute_stream(&tool, "{}").unwrap();
        use futures_util::StreamExt;
        let progress = futures_executor::block_on(stream.next());
        match progress {
            Some(tools::Progress::Done(output)) => {
                assert!(output.contains("activated"));
            }
            other => panic!("expected Done, got {other:?}"),
        }
        assert!(state.active.load(Ordering::SeqCst));
    }

    #[test]
    fn test_noop_when_already_active() {
        let plan_file = make_plan_file();
        let state = Arc::new(PlanModeState::default());
        state.active.store(true, Ordering::SeqCst);
        let tool = EnterPlanModeTool::new(state.clone(), plan_file);

        let mut stream = Tool::execute_stream(&tool, "{}").unwrap();
        use futures_util::StreamExt;
        let progress = futures_executor::block_on(stream.next());
        match progress {
            Some(tools::Progress::Done(output)) => {
                assert!(output.contains("Already in plan mode"));
            }
            other => panic!("expected Done, got {other:?}"),
        }
        assert!(state.active.load(Ordering::SeqCst));
    }

    #[test]
    fn test_invalid_json_rejected() {
        let plan_file = make_plan_file();
        let tool = EnterPlanModeTool::new(Arc::new(PlanModeState::default()), plan_file);
        let err = Tool::execute_stream(&tool, "garbage").unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }
}
