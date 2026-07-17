//! [`ExitPlanModeTool`] — lets the LLM submit a plan for user approval and
//! exit plan mode.
//!
//! When the tool is called, it reads the plan file, presents its content
//! to the user via an interactive prompt ([`InterventionRequired`]), and
//! waits for approval.  If approved, plan mode is deactivated and full
//! tool access is restored.
//!
//! Users can also exit plan mode manually via the `/approve` or `/plan`
//! slash commands.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use engine::{AgentEvent, InterventionRequest, InterventionResponse};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::mpsc;
use tools::{ProgressStream, ToolError, tool};

use engine::{ResponseRouter, next_request_id};

use crate::hooks::PlanModeState;

// ── Args ────────────────────────────────────────────────────────────────────

/// Empty args — the tool takes no parameters.
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExitPlanModeArgs {}

// ── Tool ────────────────────────────────────────────────────────────────────

/// Exits plan mode after presenting the plan to the user for approval.
///
/// Reads the plan file (`.loomis/plan.md`), shows it to the user in an
/// interactive approval prompt, and deactivates plan mode if approved.
///
/// # Response
///
/// - **Approved** → plan mode deactivated, full access restored
/// - **Suggest changes** → stays in plan mode, user feedback returned so
///   the LLM can revise the plan and call exit_plan_mode again
/// - **Cancelled** → stays in plan mode, error returned to LLM
/// - **Not in plan mode** → error
#[tool(
    name = "exit_plan_mode",
    description = "Exit plan mode and present your plan to the user for approval. \
         This reads the plan file, shows it to the user, and asks them to approve, \
         suggest changes, or cancel.\n\n\
         If the user suggests changes, their feedback is returned to you so you \
         can revise the plan and call exit_plan_mode again.\n\n\
         When to use:\n\
         - You have finished researching and written a plan to the plan file\n\
         - You are ready to present your findings for user review\n\
         - The user has asked you to exit plan mode or present your plan\n\n\
         When NOT to use:\n\
         - You are not in plan mode (this will return an error)\n\
         - You have not written a plan yet — write your plan first using the \
           write tool targeting .loomis/plan.md\n\
         - The user has not asked to see your plan and you're still researching",
    args = ExitPlanModeArgs
)]
pub struct ExitPlanModeTool {
    /// Shared plan-mode toggle between tool, hook, and TUI.
    plan_mode: Arc<PlanModeState>,
    /// Absolute path to the plan file — read and presented to the user.
    plan_file_path: PathBuf,
    /// Sender for agent events — used to emit InterventionRequired.
    agent_tx: OnceLock<mpsc::UnboundedSender<AgentEvent>>,
    /// Shared router for receiving the user's response.
    response_router: Arc<ResponseRouter>,
}

impl ExitPlanModeTool {
    /// Creates a new tool that shares the given plan-mode state and response router.
    pub fn new(
        plan_mode: Arc<PlanModeState>,
        plan_file_path: PathBuf,
        response_router: Arc<ResponseRouter>,
    ) -> Self {
        Self {
            plan_mode,
            plan_file_path,
            agent_tx: OnceLock::new(),
            response_router,
        }
    }

    /// Called by `build_coding_agent` after the agent-event channel is
    /// created.  Must be set before the tool can be used.
    pub fn set_agent_tx(&self, tx: mpsc::UnboundedSender<AgentEvent>) {
        let _ = self.agent_tx.set(tx);
    }

    fn execute_stream(&self, _args: ExitPlanModeArgs) -> Result<ProgressStream, ToolError> {
        // Guard: must be in plan mode.
        if !self.plan_mode.active.load(Ordering::SeqCst) {
            return Err(ToolError::Execution(
                "Not in plan mode. Use enter_plan_mode or /plan first to enter plan mode.".into(),
            ));
        }

        // Read the plan file content.
        let plan_content = std::fs::read_to_string(&self.plan_file_path)
            .unwrap_or_else(|e| format!("(Could not read plan file: {e})"));

        let is_empty = plan_content.trim().is_empty();
        let display_content = if is_empty {
            "(Plan file is empty — no plan was written.)".to_string()
        } else {
            plan_content
        };

        // Build the intervention request.
        let request_id = next_request_id();
        let description = format!(
            "The agent has completed its plan and requests approval to proceed.\n\n\
             ─── Plan File: {} ───\n\n\
             {display_content}\n\n\
             ─── End of Plan ───",
            self.plan_file_path.display()
        );

        // Create a per-request rendezvous channel and register with the router.
        let (tx, rx) = std::sync::mpsc::sync_channel::<InterventionResponse>(0);
        self.response_router.register(request_id.clone(), tx);

        // Send the intervention request to the TUI.
        if let Some(agent_tx) = self.agent_tx.get() {
            let _ = agent_tx.send(AgentEvent::InterventionRequired(InterventionRequest {
                request_id: request_id.clone(),
                title: "Approve Plan?".into(),
                description,
                options: vec![
                    "Approve".into(),
                    "Suggest changes…".into(),
                    "Cancel".into(),
                ],
            }));
        }

        // Block until the user responds (5-minute timeout).
        let response = match rx.recv_timeout(Duration::from_secs(300)) {
            Ok(resp) => resp,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.response_router.unregister(&request_id);
                return Err(ToolError::Execution(
                    "Timed out waiting for plan approval (5 minutes). Staying in plan mode.".into(),
                ));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(ToolError::Execution(
                    "Intervention channel disconnected (TUI may have exited). Staying in plan mode."
                        .into(),
                ));
            }
        };

        // Cleanup (no-op if the TUI's route() already removed the entry).
        self.response_router.unregister(&request_id);

        match response.chosen {
            // User pressed Esc / cancelled.
            None => Err(ToolError::Execution(
                "Plan review was cancelled. Staying in plan mode. You can revise \
                 the plan and call exit_plan_mode again, or the user can use \
                 /approve to exit plan mode manually."
                    .into(),
            )),
            // "Approve" (index 0) — deactivate plan mode.
            Some(0) => {
                self.plan_mode.active.store(false, Ordering::SeqCst);
                let msg = if is_empty {
                    "Plan approved! Plan mode deactivated. Full access restored. \
                     (Note: the plan file was empty.)"
                        .into()
                } else {
                    "Plan approved! Plan mode deactivated. Full access restored. \
                     You can now execute the plan."
                        .into()
                };
                Ok(ProgressStream::done(msg))
            }
            // "Suggest changes…" (index 1) — stay in plan mode, pass
            // feedback back to the LLM so it can revise the plan.
            Some(1) => {
                let feedback = response
                    .custom_text
                    .unwrap_or_else(|| "(No specific feedback provided.)".into());
                Ok(ProgressStream::done(format!(
                    "The user reviewed your plan and provided suggestions. \
                     You are still in plan mode.\n\n\
                     ─── User Feedback ───\n\n\
                     {feedback}\n\n\
                     ─── Instructions ──\n\n\
                     1. Read the feedback carefully — it tells you what to change or improve.\n\
                     2. Update the plan file ({plan_path}) to address each point.\n\
                     3. Use read/grep/glob as needed to research anything new.\n\
                     4. When the plan is updated, call exit_plan_mode again to present it.",
                    plan_path = self.plan_file_path.display()
                )))
            }
            // "Cancel" (index 2) — stay in plan mode.
            Some(2) => Err(ToolError::Execution(
                "Plan was not approved. Staying in plan mode. You can revise \
                 the plan and call exit_plan_mode again, or the user can use \
                 /approve to exit plan mode manually."
                    .into(),
            )),
            // Unknown option — shouldn't happen, but be defensive.
            Some(idx) => Err(ToolError::Execution(format!(
                "Unexpected response option {idx}. Staying in plan mode."
            ))),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tools::Tool;

    fn make_plan_file() -> PathBuf {
        let tmp = std::env::temp_dir().join("loomis-exit-plan-test");
        let _ = std::fs::create_dir_all(&tmp);
        tmp.join(".loomis").join("plan.md")
    }

    fn make_router() -> Arc<ResponseRouter> {
        Arc::new(ResponseRouter::new())
    }

    #[test]
    fn test_name() {
        let plan_file = make_plan_file();
        let tool = ExitPlanModeTool::new(Arc::new(PlanModeState::default()), plan_file, make_router());
        assert_eq!(tool.name(), "exit_plan_mode");
    }

    #[test]
    fn test_description() {
        let plan_file = make_plan_file();
        let tool = ExitPlanModeTool::new(Arc::new(PlanModeState::default()), plan_file, make_router());
        assert!(tool.description().contains("plan mode"));
    }

    #[test]
    fn test_parameters_schema() {
        let plan_file = make_plan_file();
        let tool = ExitPlanModeTool::new(Arc::new(PlanModeState::default()), plan_file, make_router());
        let params = tool.parameter_schema();
        assert_eq!(params["type"], "object");
        assert_eq!(params["additionalProperties"], false);
    }

    #[test]
    fn test_error_when_not_in_plan_mode() {
        let plan_file = make_plan_file();
        let tool = ExitPlanModeTool::new(
            Arc::new(PlanModeState::default()), // active defaults to false
            plan_file,
            make_router(),
        );

        let err = Tool::execute_stream(&tool, "{}").unwrap_err();
        assert!(
            matches!(err, ToolError::Execution(ref msg) if msg.contains("Not in plan mode")),
            "expected 'Not in plan mode' error, got: {err:?}"
        );
    }

    #[test]
    fn test_agent_tx_not_set_is_handled_gracefully() {
        // This test verifies that when agent_tx is not set, the tool
        // doesn't panic when trying to send. However, since there's no
        // sender, no InterventionRequired event will be emitted, and
        // the blocking recv will eventually timeout (300s).
        //
        // We can't practically test the full blocking path in a unit
        // test, but we verify that the tool constructs correctly and
        // the guard clause works.
        let plan_file = make_plan_file();
        let state = Arc::new(PlanModeState::default());
        state.active.store(true, Ordering::SeqCst);
        let tool = ExitPlanModeTool::new(state, plan_file, make_router());
        // agent_tx is NOT set
        assert!(tool.agent_tx.get().is_none());
        // The tool should still be functional — just can't send events.
        // We don't call execute_stream here to avoid the 300s timeout.
    }

    #[test]
    fn test_invalid_json_rejected() {
        let plan_file = make_plan_file();
        let tool = ExitPlanModeTool::new(Arc::new(PlanModeState::default()), plan_file, make_router());
        let err = Tool::execute_stream(&tool, "garbage").unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_extra_field_rejected() {
        let plan_file = make_plan_file();
        let tool = ExitPlanModeTool::new(Arc::new(PlanModeState::default()), plan_file, make_router());
        let err = Tool::execute_stream(&tool, r#"{"extra": true}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }
}
