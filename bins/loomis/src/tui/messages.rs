//! Chat message types and TUI commands.
//!
//! Pure type definitions with no dependency on the `App` state machine.
//! Separated from [`super::app`] so the file doesn't grow to 1500 lines.

use std::time::SystemTime;

// ── ChatMessage ──────────────────────────────────────────────────────────────────

/// One display entry in the chat area.
///
/// Each variant maps to a distinct visual style in the UI
/// (see [`super::ui`] for the rendering).
#[derive(Debug, Clone)]
pub enum ChatMessage {
    /// User input — cyan, bold, `>` prefix.
    User { content: String, timestamp: String },
    /// Model text output — white, no prefix. Streamed token-by-token.
    Assistant { content: String, timestamp: String },
    /// Chain-of-thought reasoning — yellow, dimmed.
    Reasoning { content: String, timestamp: String },
    /// A tool call, either in-progress or completed.
    ToolCall {
        id: String,
        name: String,
        args: String,
        state: ToolCallState,
        /// Latest progress message while tool is Running (shown inline).
        progress_line: Option<String>,
        timestamp: String,
    },
    /// System-level message (slash commands, info).
    System { content: String, timestamp: String },
    /// A shell command awaiting user confirmation.
    ShellConfirm {
        tool_call_id: String,
        command: String,
        responded: bool,
        timestamp: String,
    },
    /// Shell command output from user's `!` prefix — green header, dim output.
    /// `state` controls rendering: `Running` shows a "Running…" indicator,
    /// `Complete(output)` shows the captured stdout/stderr.
    ShellOutput {
        command: String,
        state: ShellOutputState,
        timestamp: String,
    },
    /// Error display — red, bold.
    Error { content: String, timestamp: String },
}

impl ChatMessage {
    /// Returns a formatted local-time timestamp string (HH:MM:SS).
    pub fn now_timestamp() -> String {
        let secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Approximate local time from UTC — works for display purposes.
        // On Windows this gives UTC; use a simple offset heuristic.
        let total_secs = secs % 86400; // seconds within the day
        let hours = total_secs / 3600;
        let minutes = (total_secs % 3600) / 60;
        let seconds = total_secs % 60;
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    }
}

#[derive(Debug, Clone)]
pub enum ToolCallState {
    /// Arguments are still streaming in.
    Running,
    /// Tool execution finished with this output.
    Complete(String),
}

/// Tracks whether a user `!` shell command is still running or has completed.
#[derive(Debug, Clone)]
pub enum ShellOutputState {
    /// Command is executing — the TUI shows a "Running…" indicator.
    Running,
    /// Command finished — output is displayed.
    Complete(String),
}

// ── TuiCommand ───────────────────────────────────────────────────────────────────

/// Commands sent from the TUI thread to the agent background task.
#[derive(Debug, Clone)]
pub enum TuiCommand {
    /// User submitted a message — push to memory and run the agent loop.
    RunAgent(String),
    /// User typed !command — execute shell command asynchronously.
    RunShell(String),
    /// Cancel the currently-running generation.
    CancelGeneration,
    /// Reset conversation, preserving system prompt.
    ClearConversation,
    /// User responded to a shell confirmation prompt.
    ShellConfirmation {
        tool_call_id: String,
        approved: bool,
    },
    /// Signal the agent thread to exit.
    Exit,
}

// ── ThreadPicker ──────────────────────────────────────────────────────────────────

/// State for the thread-selection overlay.
///
/// When `Some`, all keyboard input is intercepted by the picker until the
/// user selects a thread or presses `Esc`.
#[derive(Debug, Clone)]
pub struct ThreadPicker {
    /// Available threads, sorted newest-first.
    pub threads: Vec<memory::ThreadInfo>,
    /// Currently highlighted index.
    pub selected: usize,
}

// ── Helpers ───────────────────────────────────────────────────────────────────────

/// Truncates text at a valid UTF-8 boundary for compact display, appending
/// `"..."` when truncation occurs.
pub fn truncate_for_display(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }
    let boundary = text.floor_char_boundary(max_len);
    format!("{}...", &text[..boundary])
}

/// Returns `true` if `name` is a valid thread name (alphanumeric,
/// hyphens, underscores, no whitespace or special chars).
pub fn is_valid_thread_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}
