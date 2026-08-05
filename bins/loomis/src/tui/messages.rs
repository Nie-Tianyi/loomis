//! Chat message types and TUI commands.
//!
//! Pure type definitions with no dependency on the `App` state machine.
//! Separated from [`super::app`] so the file doesn't grow to 1500 lines.

use engine::{CallOrigin, InterventionResponse};
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
    /// A tool call or user command, either in-progress or completed.
    /// The [`origin`](CallOrigin) field distinguishes LLM tool calls
    /// from user-initiated `!command` invocations.
    ToolCall {
        id: String,
        name: String,
        args: String,
        state: ToolCallState,
        origin: CallOrigin,
        /// Accumulated progress messages while tool is Running.
        /// Each [`ToolProgress`](engine::AgentEvent::ToolProgress) event
        /// appends a new line; all are rendered indented under the header.
        progress_lines: Vec<String>,
        timestamp: String,
    },
    /// System-level message (slash commands, info).
    System { content: String, timestamp: String },
    /// Startup welcome banner — ASCII logo + mascot 小织, rendered by
    /// [`super::welcome`]. Seeded once by [`super::app::App::new`]; carries
    /// no timestamp (a banner is not a conversational message).
    Welcome { model: String, workspace: String },
    /// A hook is requesting user intervention — rendered as an
    /// interactive prompt with navigable options.
    Intervene {
        request_id: String,
        title: String,
        description: String,
        options: Vec<String>,
        responded: bool,
        /// Index of the chosen option after the user responds.
        chosen: Option<usize>,
        /// Custom text if the user picked the "…"-suffixed option.
        custom_text: Option<String>,
        timestamp: String,
    },
    /// Error display — red, bold.
    Error { content: String, timestamp: String },
}

/// Returns the local timezone's offset from UTC in seconds.
/// Positive = east of UTC (e.g., +28800 for UTC+8).
/// Cross-platform (Windows / macOS / Linux) and DST-aware via the `time`
/// crate (already in the dependency tree through `tracing-appender`).
/// Cached: queried once on first call, reused thereafter.
fn local_utc_offset_seconds() -> i64 {
    use std::sync::OnceLock;
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        time::UtcOffset::current_local_offset()
            .map(|offset| offset.whole_seconds() as i64)
            .unwrap_or(0)
    })
}

impl ChatMessage {
    /// Returns a formatted local-time timestamp string (HH:MM:SS).
    pub fn now_timestamp() -> String {
        let secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let offset = local_utc_offset_seconds();
        let local_secs = (secs + offset) % 86400;
        // Handle negative wrap (shouldn't happen with realistic offsets).
        let total_secs = if local_secs < 0 {
            local_secs + 86400
        } else {
            local_secs
        } as u64;
        let hours = total_secs / 3600;
        let minutes = (total_secs % 3600) / 60;
        let seconds = total_secs % 60;
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    }
}

#[derive(Debug, Clone)]
pub enum ToolCallState {
    /// Arguments are still streaming in, or tool is executing.
    Running,
    /// Tool execution completed successfully with this output.
    Complete(String),
    /// A hook rejected the tool before execution (e.g. sandbox policy).
    Rejected(String),
    /// Tool execution failed with this error.
    Error(String),
}

// ── SelectionState ───────────────────────────────────────────────────────────────

/// Tracks mouse-based text selection in the chat area.
///
/// Start and end are `(line, column)` positions: line indices into the
/// wrapped `all_lines` vec produced by [`super::ui::wrap_to_width`], and
/// **display columns** within those lines (CJK characters count as two
/// columns, matching what the user sees on screen). When `dragging` is
/// `true`, the user is still holding the mouse button and the selection
/// updates live. When `false`, the selection is stable — Ctrl+C copies it.
#[derive(Debug, Clone)]
pub struct SelectionState {
    /// Start line index in the wrapped `all_lines` vec (inclusive).
    pub start_line: usize,
    /// Display column within `start_line` where the selection begins.
    pub start_col: usize,
    /// End line index (inclusive). May be less than `start_line` while
    /// dragging upward; normalized to `start_line ≤ end_line` on mouse up.
    pub end_line: usize,
    /// Display column within `end_line` where the selection ends.
    pub end_col: usize,
    /// `true` while the user is still holding the left mouse button.
    pub dragging: bool,
}

impl SelectionState {
    /// Returns `(start_line, start_col, end_line, end_col)` in document
    /// order, regardless of which direction the user dragged.
    pub fn ordered_bounds(&self) -> (usize, usize, usize, usize) {
        let dragged_downward = self.start_line < self.end_line
            || (self.start_line == self.end_line && self.start_col <= self.end_col);
        if dragged_downward {
            (self.start_line, self.start_col, self.end_line, self.end_col)
        } else {
            (self.end_line, self.end_col, self.start_line, self.start_col)
        }
    }
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
    /// User responded to an intervention prompt.
    InterventionResponse {
        request_id: String,
        response: InterventionResponse,
    },
    /// Signal the agent thread to exit.
    Exit,
}

// ── Slash Completion ─────────────────────────────────────────────────────────────

/// Static metadata for one slash command — drives the completion popup
/// (Nielsen #6: recognition rather than recall).
#[derive(Debug)]
pub struct CommandInfo {
    /// Command name without the leading `/`.
    pub name: &'static str,
    /// Usage line shown in the popup, e.g. `/save <name>`.
    pub usage: &'static str,
    /// One-line description shown dimmed next to the usage.
    pub desc: &'static str,
}

/// All slash commands in display order.
pub static SLASH_COMMANDS: &[CommandInfo] = &[
    CommandInfo {
        name: "exit",
        usage: "/exit",
        desc: "Quit the application",
    },
    CommandInfo {
        name: "new",
        usage: "/new",
        desc: "Start a new conversation",
    },
    CommandInfo {
        name: "init",
        usage: "/init [text]",
        desc: "Initialize project rules (LOOMIS.md); text = extra instruction",
    },
    CommandInfo {
        name: "plan",
        usage: "/plan [text]",
        desc: "Toggle plan mode; with text, enter plan mode and make the plan",
    },
    CommandInfo {
        name: "approve",
        usage: "/approve",
        desc: "Approve plan and exit plan mode",
    },
    CommandInfo {
        name: "save",
        usage: "/save <name>",
        desc: "Save conversation as a named thread",
    },
    CommandInfo {
        name: "resume",
        usage: "/resume [name]",
        desc: "Restore a saved thread",
    },
    CommandInfo {
        name: "threads",
        usage: "/threads",
        desc: "Open the thread picker",
    },
    CommandInfo {
        name: "stats",
        usage: "/stats",
        desc: "Show memory statistics",
    },
    CommandInfo {
        name: "tools",
        usage: "/tools",
        desc: "List registered tools",
    },
    CommandInfo {
        name: "skill",
        usage: "/skill <name>",
        desc: "Load a named skill",
    },
    CommandInfo {
        name: "help",
        usage: "/help",
        desc: "Show the help message",
    },
];

/// State for the slash-command completion popup.
///
/// `Some` while the input starts with `/` and at least one command matches
/// the typed prefix. Keyboard input is intercepted until the popup is
/// accepted (Tab/Enter) or dismissed (Esc).
#[derive(Debug, Clone)]
pub struct SlashCompletionState {
    /// Commands matching the current input prefix.
    pub matches: Vec<&'static CommandInfo>,
    /// Currently highlighted index in `matches`.
    pub selected: usize,
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

/// Returns `true` if `name` is a valid thread name.
///
/// Delegates to [`memory::sanitize_filename`] for the canonical check, so
/// any name that passes validation will be preserved verbatim by the
/// persistence layer.  Control characters and filesystem-illegal characters
/// (`/`, `\`, `:`, `*`, `?`, `"`, `<`, `>`, `|`) are rejected.
pub fn is_valid_thread_name(name: &str) -> bool {
    !name.is_empty() && name == memory::sanitize_filename(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses `HH:MM:SS` into seconds-of-day.
    fn secs_of_day(ts: &str) -> u64 {
        let mut it = ts.split(':').map(|p| p.parse::<u64>().unwrap());
        let h = it.next().unwrap();
        let m = it.next().unwrap();
        let s = it.next().unwrap();
        h * 3600 + m * 60 + s
    }

    /// `now_timestamp()` must match local wall-clock time (any timezone).
    /// Allows a 1-second skew for the boundary between the two calls.
    #[test]
    fn now_timestamp_matches_local_time() {
        let ts = ChatMessage::now_timestamp();
        let local = time::OffsetDateTime::now_local().expect("local offset available");
        let expected =
            (local.hour() as u64) * 3600 + (local.minute() as u64) * 60 + local.second() as u64;
        let actual = secs_of_day(&ts);
        let diff = actual.abs_diff(expected);
        assert!(
            diff <= 1 || diff >= 86399,
            "now_timestamp() = {ts} but local time = {expected}s of day"
        );
    }
}
