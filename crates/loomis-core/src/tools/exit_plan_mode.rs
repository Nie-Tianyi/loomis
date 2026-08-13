//! [`ExitPlanModeTool`] — lets the LLM submit a plan for user approval and
//! exit plan mode.
//!
//! When the tool is called, it reads the plan file, presents its content
//! to the user via an interactive prompt ([`InterventionRequired`]), and
//! waits for approval.  If approved, the plan is **archived** to
//! `.loomis/plan/<summary>.md` and plan mode is deactivated.
//!
//! Users can also exit plan mode manually via the `/approve` or `/plan`
//! slash commands.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use agent_oxide::engine::AgentEvent;
use agent_oxide::tools::{ProgressStream, ToolError, tool};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::mpsc;

use agent_oxide::engine::ResponseRouter;
use agent_oxide::engine::intervention::{self, InterventionError};

use crate::hooks::PlanModeState;

// ── Plan archiving ──────────────────────────────────────────────────────────

/// Extract a human-readable plan summary from the plan content.
///
/// Uses the first `# Heading` line (without the `# ` prefix) as the
/// summary. Falls back to the first non-empty, non-code-fence line.
/// Truncated to 64 chars after sanitisation.
pub(crate) fn extract_plan_summary(content: &str) -> String {
    // Try the first # heading.
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("# ") {
            let s = sanitize_for_filename(heading);
            if !s.is_empty() {
                return truncate_summary(&s);
            }
        }
    }

    // Fallback: first non-empty, non-code-fence line.
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with("```") {
            let s = sanitize_for_filename(trimmed);
            if !s.is_empty() {
                return truncate_summary(&s);
            }
        }
    }

    "untitled-plan".to_string()
}

/// Sanitize a string for use as a filename component.
fn sanitize_for_filename(s: &str) -> String {
    let s = s.to_lowercase();
    let s: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Collapse consecutive dashes, trim leading/trailing dashes.
    let parts: Vec<&str> = s.split('-').filter(|p| !p.is_empty()).collect();
    parts.join("-")
}

/// Truncate a summary string to at most 64 characters.
fn truncate_summary(s: &str) -> String {
    if s.len() <= 64 {
        s.to_string()
    } else {
        // Try to truncate at a dash boundary.
        let end = s[..64].rfind('-').unwrap_or(64);
        s[..end].to_string()
    }
}

/// Archive plan content to `.loomis/plan/<summary>.md`.
///
/// Returns the path of the archived plan file.
/// Handles filename collisions by appending `-2`, `-3`, etc.
pub(crate) fn archive_plan(content: &str, plan_dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(plan_dir)?;

    let summary = extract_plan_summary(content);
    let base = summary;

    // Find an available filename (avoid collisions).
    let mut candidate = plan_dir.join(format!("{base}.md"));
    if !candidate.exists() {
        std::fs::write(&candidate, content)?;
        return Ok(candidate);
    }

    for n in 2u32.. {
        candidate = plan_dir.join(format!("{base}-{n}.md"));
        if !candidate.exists() {
            std::fs::write(&candidate, content)?;
            return Ok(candidate);
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "Could not find an available plan filename after 4 billion attempts",
    ))
}

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
         suggest changes, or cancel. On approval, the plan is automatically archived \
         to .loomis/plan/<summary>.md so past plans are never lost.\n\n\
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
    /// Shared plan-mode toggle between tool, hook, and frontend.
    plan_mode: Arc<PlanModeState>,
    /// Absolute path to the plan file — read and presented to the user.
    plan_file_path: PathBuf,
    /// Directory where approved plans are archived (`.loomis/plan/`).
    plan_dir: PathBuf,
    /// Sender for agent events — used to emit InterventionRequired.
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
    /// Shared router for receiving the user's response.
    response_router: Arc<ResponseRouter>,
}

impl ExitPlanModeTool {
    /// Creates a new tool that shares the given plan-mode state, response
    /// router, and the agent-event channel (needed to emit
    /// `InterventionRequired`).
    pub fn new(
        plan_mode: Arc<PlanModeState>,
        plan_file_path: PathBuf,
        plan_dir: PathBuf,
        response_router: Arc<ResponseRouter>,
        agent_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Self {
        Self {
            plan_mode,
            plan_file_path,
            plan_dir,
            agent_tx,
            response_router,
        }
    }

    fn execute_stream(&self, _args: ExitPlanModeArgs) -> Result<ProgressStream, ToolError> {
        // Guard: must be in plan mode.
        if !self.plan_mode.active.load(Ordering::SeqCst) {
            tracing::warn!("exit_plan_mode called while not in plan mode");
            return Err(ToolError::Execution(
                "Not in plan mode. Use enter_plan_mode or /plan first to enter plan mode.".into(),
            ));
        }

        // Read the plan file content (cloned so we can use it after the display copy).
        let plan_content = std::fs::read_to_string(&self.plan_file_path)
            .unwrap_or_else(|e| format!("(Could not read plan file: {e})"));
        let plan_content_for_archive = plan_content.clone();

        let is_empty = plan_content.trim().is_empty();
        let display_content = if is_empty {
            "(Plan file is empty — no plan was written.)".to_string()
        } else {
            plan_content
        };

        let description = format!(
            "The agent has completed its plan and requests approval to proceed.\n\n\
             ─── Plan File: {} ───\n\n\
             {display_content}\n\n\
             ─── End of Plan ───",
            self.plan_file_path.display()
        );

        // Delegate the common request/response plumbing to the shared helper.
        let response = intervention::request_intervention(
            &self.response_router,
            &self.agent_tx,
            "Approve Plan?".into(),
            description,
            vec!["Approve".into(), "Suggest changes…".into(), "Cancel".into()],
            Duration::from_secs(300),
        );

        let response = match response {
            Ok(resp) => resp,
            Err(InterventionError::Timeout) => {
                tracing::error!(
                    path = %self.plan_file_path.display(),
                    "Timed out waiting for plan approval"
                );
                return Err(ToolError::Execution(
                    "Timed out waiting for plan approval (5 minutes). Staying in plan mode.".into(),
                ));
            }
            Err(InterventionError::Disconnected) => {
                tracing::error!(
                    "Intervention channel disconnected (TUI may have exited) during plan approval"
                );
                return Err(ToolError::Execution(
                    "Intervention channel disconnected (TUI may have exited). Staying in plan mode."
                        .into(),
                ));
            }
        };

        match response.chosen {
            // User pressed Esc / cancelled.
            None => {
                tracing::warn!("Plan review cancelled by user, staying in plan mode");
                Err(ToolError::Execution(
                    "Plan review was cancelled. Staying in plan mode. You can revise \
                     the plan and call exit_plan_mode again, or the user can use \
                     /approve to exit plan mode manually."
                        .into(),
                ))
            }
            // "Approve" (index 0) — archive the plan, then deactivate plan mode.
            Some(0) => {
                // Archive the plan before deactivating.
                let archive_result = if is_empty {
                    None
                } else {
                    match archive_plan(&plan_content_for_archive, &self.plan_dir) {
                        Ok(archived_path) => Some(archived_path),
                        Err(e) => {
                            // If archiving fails, still deactivate — don't
                            // trap the user in plan mode over a disk error.
                            tracing::error!(
                                error = %e,
                                "Failed to archive plan; plan mode deactivated anyway"
                            );
                            self.plan_mode.active.store(false, Ordering::SeqCst);
                            return Err(ToolError::Execution(format!(
                                "Plan approved, but failed to archive the plan: {e}. \
                                 Plan mode deactivated anyway."
                            )));
                        }
                    }
                };

                self.plan_mode.active.store(false, Ordering::SeqCst);
                tracing::info!(
                    archived = archive_result.as_ref().map(|p| p.display().to_string()),
                    "Plan approved, plan mode deactivated"
                );
                let msg = if let Some(ref archived_path) = archive_result {
                    format!(
                        "Plan approved! Plan mode deactivated. Full access restored. \
                         Plan archived to: {}\n\
                         You can now execute the plan.",
                        archived_path.display()
                    )
                } else {
                    "Plan approved! Plan mode deactivated. Full access restored. \
                     (Note: the plan file was empty — nothing to archive.)"
                        .into()
                };
                Ok(ProgressStream::done(msg))
            }
            // "Suggest changes…" (index 1) — stay in plan mode, pass
            // feedback back to the LLM so it can revise the plan.
            Some(1) => {
                tracing::info!("Plan review returned suggestions, staying in plan mode");
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
            Some(2) => {
                tracing::warn!("Plan not approved, staying in plan mode");
                Err(ToolError::Execution(
                    "Plan was not approved. Staying in plan mode. You can revise \
                     the plan and call exit_plan_mode again, or the user can use \
                     /approve to exit plan mode manually."
                        .into(),
                ))
            }
            // Unknown option — shouldn't happen, but be defensive.
            Some(idx) => {
                tracing::warn!(idx, "Unexpected plan approval response option");
                Err(ToolError::Execution(format!(
                    "Unexpected response option {idx}. Staying in plan mode."
                )))
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use agent_oxide::tools::Tool;

    fn make_plan_file() -> PathBuf {
        let tmp = std::env::temp_dir().join("loomis-exit-plan-test");
        let _ = std::fs::create_dir_all(&tmp);
        tmp.join(".loomis").join("plan.md")
    }

    fn make_plan_dir() -> PathBuf {
        let tmp = std::env::temp_dir().join("loomis-exit-plan-test");
        let _ = std::fs::create_dir_all(&tmp);
        tmp.join(".loomis").join("plan")
    }

    fn make_router() -> Arc<ResponseRouter> {
        Arc::new(ResponseRouter::new())
    }

    /// A live event channel — the tool now requires the sender at
    /// construction (no more back-door `set_agent_tx`).
    fn make_agent_tx() -> mpsc::UnboundedSender<AgentEvent> {
        let (tx, _rx) = mpsc::unbounded_channel();
        tx
    }

    fn make_tool() -> ExitPlanModeTool {
        ExitPlanModeTool::new(
            Arc::new(PlanModeState::default()),
            make_plan_file(),
            make_plan_dir(),
            make_router(),
            make_agent_tx(),
        )
    }

    #[test]
    fn test_name() {
        assert_eq!(make_tool().name(), "exit_plan_mode");
    }

    #[test]
    fn test_description() {
        assert!(make_tool().description().contains("plan mode"));
    }

    #[test]
    fn test_parameters_schema() {
        let params = make_tool().parameter_schema();
        assert_eq!(params["type"], "object");
        assert_eq!(params["additionalProperties"], false);
    }

    #[test]
    fn test_error_when_not_in_plan_mode() {
        let tool = ExitPlanModeTool::new(
            Arc::new(PlanModeState::default()), // active defaults to false
            make_plan_file(),
            make_plan_dir(),
            make_router(),
            make_agent_tx(),
        );

        let err = Tool::execute_stream(&tool, "{}").unwrap_err();
        assert!(
            matches!(err, ToolError::Execution(ref msg) if msg.contains("Not in plan mode")),
            "expected 'Not in plan mode' error, got: {err:?}"
        );
    }

    #[test]
    fn test_invalid_json_rejected() {
        let err = Tool::execute_stream(&make_tool(), "garbage").unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_extra_field_rejected() {
        let err = Tool::execute_stream(&make_tool(), r#"{"extra": true}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }
}
