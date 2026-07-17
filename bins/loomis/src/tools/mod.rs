//! Concrete tool implementations.
//!
//! Shared utilities for concrete tools live here.

mod ask_user_question;
mod calculator;
mod edit;
mod enter_plan_mode;
mod exit_plan_mode;
mod glob;
mod grep;
mod ls;
mod read;
mod shell;
mod skill_tool;
mod todo;
mod write;

pub use ask_user_question::AskUserQuestionTool;
pub use calculator::CalculatorTool;
pub use edit::EditTool;
pub use enter_plan_mode::EnterPlanModeTool;
pub use exit_plan_mode::ExitPlanModeTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use ls::LsTool;
pub use read::ReadTool;
pub use shell::ShellTool;
pub use skill_tool::SkillTool;
pub use todo::{TODO_MARKER, TodoItem, TodoTool};
pub use write::WriteTool;

// ── Shared helpers ─────────────────────────────────────────────────────────

/// Build a single-line content preview for progress display.
///
/// Shows the first non-empty line, truncated to 80 characters.
/// Appends a line-count hint for multi-line content.
pub(crate) fn content_preview(content: &str, prefix: &str) -> String {
    if content.is_empty() {
        return String::new();
    }

    let first_line = content.lines().next().unwrap_or("");
    let line_count = content.lines().count();

    let truncated = if first_line.len() > 80 {
        let boundary = first_line.floor_char_boundary(77);
        format!("{}...", &first_line[..boundary])
    } else {
        first_line.to_string()
    };

    if line_count > 1 {
        format!("{prefix}: {} (+{} more lines)", truncated, line_count - 1)
    } else {
        format!("{prefix}: {}", truncated)
    }
}
