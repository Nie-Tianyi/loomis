//! [`AskUserQuestionTool`] — lets the LLM ask the user a question.
//!
//! # How it works
//!
//! The tool pauses the agent and shows an interactive prompt in the TUI
//! via the existing [`InterventionRequired`](agent_oxide::engine::AgentEvent::InterventionRequired)
//! mechanism.  The user navigates options (or types free-form text) and
//! their response is returned as the tool output.
//!
//! # Comparison with SandboxHook
//!
//! | Aspect | SandboxHook | AskUserQuestionTool |
//! |--------|-------------|---------------------|
//! | Trigger point | `before_tool_call` hook | `execute_stream` (during tool exec) |
//! | Who initiates | Shell tool call by LLM | LLM calls this tool directly |
//! | Purpose | Security approval | Information gathering |
//! | Options | Fixed (Approve/Deny/Other) | LLM-defined |
//! | Timeout | 2 minutes | 5 minutes |

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use agent_oxide::engine::AgentEvent;
use agent_oxide::tools::{ProgressStream, ToolError, tool};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::mpsc;

use agent_oxide::engine::ResponseRouter;
use agent_oxide::engine::intervention::{self, InterventionError};

// ── Args ────────────────────────────────────────────────────────────────────

/// Arguments for the ask_user_question tool.
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AskUserQuestionArgs {
    /// The question or prompt to show the user. Displayed prominently.
    #[schemars(
        description = "The question or prompt to show the user. Be clear and specific about what you need them to answer."
    )]
    pub question: String,

    /// Optional additional context, explanation, or background for the
    /// question. Shown below the question in a dimmer style.
    #[schemars(
        description = "Optional additional context, explanation, or background for the question."
    )]
    pub description: Option<String>,

    /// Predefined choices for the user. If omitted or empty, the user
    /// types a free-form text response. An option whose label ends with
    /// "…" (like "Other…") lets the user type custom text.
    #[schemars(
        description = "Predefined choices for the user to pick from. If omitted, the user types a free-form response. End an option with \"…\" to allow custom text input."
    )]
    pub options: Option<Vec<String>>,
}

// ── Tool ────────────────────────────────────────────────────────────────────

/// Lets the LLM ask the user a question and wait for their response.
///
/// Use this when you need the user to make a choice, provide input, or
/// answer a question that only they can answer — preferences, design
/// decisions, clarification of ambiguous requirements, confirmation of
/// actions, information only the user knows, etc.
///
/// # Parameters
///
/// ```json
/// {
///   "question": "Which approach should I use?",
///   "description": "Option A is faster, Option B is more maintainable.",
///   "options": ["Option A", "Option B", "Other…"]
/// }
/// ```
///
/// # Response
///
/// - Predefined option selected → the label text (e.g. `"Option A"`)
/// - "…" option with custom text → the custom text only
/// - User cancelled (Esc) → error
/// - Timeout after 5 minutes → error
#[tool(
    name = "ask_user_question",
    description = "Ask the user a question and wait for their response. Use this when you \
         need the user to make a choice, provide input, or answer a question that only \
         they can answer.\n\n\
         You can provide predefined options for the user to choose from, or leave \
         options empty for a free-form text response.\n\n\
         When to use:\n\
         - Asking for user preferences or design decisions\n\
         - Requesting clarification on ambiguous requirements\n\
         - Confirming potentially destructive actions\n\
         - Gathering information only the user knows\n\n\
         When NOT to use:\n\
         - Asking questions you can answer from the codebase or tools\n\
         - Asking rhetorical questions\n\
         - Asking for information that doesn't affect your next action",
    args = AskUserQuestionArgs
)]
pub struct AskUserQuestionTool {
    /// Sender for agent events — used to emit InterventionRequired.
    agent_tx: OnceLock<mpsc::UnboundedSender<AgentEvent>>,
    /// Shared router for receiving the user's response.
    response_router: Arc<ResponseRouter>,
}

impl AskUserQuestionTool {
    /// Creates a new tool that shares the given response router.
    pub fn new(response_router: Arc<ResponseRouter>) -> Self {
        Self {
            agent_tx: OnceLock::new(),
            response_router,
        }
    }

    /// Called by `build_coding_agent` after the agent-event channel is
    /// created.  Must be set before the tool can be used.
    pub fn set_agent_tx(&self, tx: mpsc::UnboundedSender<AgentEvent>) {
        let _ = self.agent_tx.set(tx);
    }

    /// Core logic — blocks the agent task until the user responds.
    fn execute_stream(&self, args: AskUserQuestionArgs) -> Result<ProgressStream, ToolError> {
        let question_preview: String = args.question.chars().take(300).collect();
        tracing::debug!(question = %question_preview, "Asking user question");

        let agent_tx = self
            .agent_tx
            .get()
            .ok_or_else(|| ToolError::Execution("Agent event channel not configured".into()))?;

        // Default to a single free-text option if none provided.
        let options: Vec<String> = args
            .options
            .clone()
            .filter(|opts| !opts.is_empty())
            .unwrap_or_else(|| vec!["Answer…".into()]);

        // Delegate the common request/response plumbing to the shared helper.
        let response = intervention::request_intervention(
            &self.response_router,
            agent_tx,
            args.question,
            args.description.unwrap_or_default(),
            options.clone(),
            Duration::from_secs(300),
        );

        let response = match response {
            Ok(resp) => resp,
            Err(InterventionError::Timeout) => {
                tracing::error!(
                    question = %question_preview,
                    "Timed out waiting for user response"
                );
                return Err(ToolError::Execution(
                    "Timed out waiting for user response (5 minutes)".into(),
                ));
            }
            Err(InterventionError::Disconnected) => {
                tracing::error!(
                    question = %question_preview,
                    "Intervention channel disconnected (TUI may have exited)"
                );
                return Err(ToolError::Execution(
                    "Intervention channel disconnected (TUI may have exited)".into(),
                ));
            }
        };

        // Build output from the user's response.
        match (response.chosen, response.custom_text) {
            (None, _) => {
                tracing::warn!(question = %question_preview, "User cancelled the question");
                Err(ToolError::Execution("User cancelled the question".into()))
            }
            (Some(_), Some(custom)) => {
                // User selected "…" option and typed custom text.
                let answer_preview: String = custom.chars().take(300).collect();
                tracing::info!(
                    question = %question_preview,
                    answer = %answer_preview,
                    "User answered question (custom text)"
                );
                Ok(ProgressStream::done(custom))
            }
            (Some(idx), None) => {
                // User selected a specific option.
                let label = options
                    .get(idx)
                    .cloned()
                    .unwrap_or_else(|| format!("Option {idx}"));
                tracing::info!(
                    question = %question_preview,
                    option = idx,
                    answer = %label,
                    "User answered question"
                );
                Ok(ProgressStream::done(label))
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use agent_oxide::tools::Tool;

    #[test]
    fn test_name() {
        let router = Arc::new(ResponseRouter::new());
        assert_eq!(AskUserQuestionTool::new(router).name(), "ask_user_question");
    }

    #[test]
    fn test_description() {
        let router = Arc::new(ResponseRouter::new());
        assert!(
            AskUserQuestionTool::new(router)
                .description()
                .contains("Ask the user")
        );
    }

    #[test]
    fn test_parameters_schema() {
        let router = Arc::new(ResponseRouter::new());
        let params = AskUserQuestionTool::new(router).parameter_schema();
        assert_eq!(params["type"], "object");
        assert!(
            params["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("question"))
        );
        assert_eq!(params["additionalProperties"], false);
    }

    #[test]
    fn test_invalid_json() {
        let router = Arc::new(ResponseRouter::new());
        let tool = AskUserQuestionTool::new(router);
        let err = Tool::execute_stream(&tool, "garbage").unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_missing_question_field() {
        let router = Arc::new(ResponseRouter::new());
        let tool = AskUserQuestionTool::new(router);
        let err = Tool::execute_stream(&tool, r#"{"wrong": "field"}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_extra_field_rejected() {
        let router = Arc::new(ResponseRouter::new());
        let tool = AskUserQuestionTool::new(router);
        let err =
            Tool::execute_stream(&tool, r#"{"question": "hello", "extra": true}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }
}
