//! # App State
//!
//! The mutable state machine for the TUI: chat messages, input buffer,
//! scrolling, streaming status, and keyboard processing.
//!
//! ## File layout
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`super::messages`] | `ChatMessage`, `TuiCommand`, `ThreadPicker` types |
//! | [`super::input`] | `handle_key()`, slash commands, shell confirmation |
//! | `app` (here) | `App` struct + event application + tests |

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use engine::{AgentEvent, CallOrigin};
use memory::{PendingHints, SharedMemory};
use observability::TraceStore;
use persistence::PersistenceConfig;
use skills::{self, SkillRegistry};

use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

use super::messages::{ChatMessage, SelectionState, ToolCallState};
use super::paste::PasteStore;
use crate::hooks::PlanModeState;
use crate::tools::TodoItem;

// ── App ──────────────────────────────────────────────────────────────────────────

/// Mutable state owned by the TUI event loop.
///
/// All fields are updated synchronously during the render/keyboard/event cycle.
/// The `memory` field is a shared `Arc<RwLock<Memory>>` clone — read-only from
/// the TUI side (for `/stats`), written exclusively by the agent thread.
pub struct App {
    // ── Conversation ──
    pub messages: Vec<ChatMessage>,
    /// Cached line count per message, rebuilt each frame.
    /// Parallel to `messages`.
    pub line_counts: Vec<usize>,

    // ── Input ──
    pub input: String,
    /// Byte offset into `input`.
    pub input_cursor: usize,
    /// Multi-line pastes referenced by `[Pasted text #N …]` placeholders
    /// inside `input`. Cleared on every submit.
    pub paste_store: PasteStore,
    /// Visual rows scrolled out of view at the top of the input area when
    /// the input exceeds the allocated height (0 = first visual row shown).
    pub input_scroll_offset: usize,

    // ── Scrolling ──
    /// How many lines the user has scrolled up (0 = bottom).
    pub scroll_offset: usize,
    /// When `true`, new messages reset scroll to bottom.
    pub auto_scroll: bool,

    // ── Agent status ──
    pub streaming: bool,
    /// When the current agent run started — `Some` while streaming.
    /// Drives the elapsed-time readout in the status bar.
    pub run_started_at: Option<Instant>,
    /// Current spinner animation frame index, advanced by the event loop.
    pub spinner_frame: usize,
    /// Last time the spinner frame was advanced.
    pub last_spinner_tick: Instant,

    // ── Shared state ──
    pub model: String,
    pub memory: SharedMemory,
    pub tool_names: Vec<String>,
    /// Shared todo list — read-only from the TUI side (written by TodoTool).
    pub todos: Arc<RwLock<Vec<TodoItem>>>,
    /// Workspace root directory for `!` shell commands.
    pub workspace_root: PathBuf,
    /// Queue for user hints injected during active agent runs.
    /// Drained by the agent loop before each LLM call.
    pub pending_hints: PendingHints,

    // ── Input history ──
    pub history: Vec<String>,
    /// `Some(idx)` while navigating history; `None` when at the current draft.
    pub history_index: Option<usize>,
    /// Saved copy of the in-progress input before history navigation started.
    pub(super) draft_input: String,

    // ── Thread picker overlay ──
    pub thread_picker: Option<super::messages::ThreadPicker>,

    // ── Slash-command completion ──
    /// Active completion popup state — `Some` while the input starts with
    /// `/` and at least one command matches the typed prefix.
    pub slash_completion: Option<super::messages::SlashCompletionState>,
    /// Set when the user dismisses the popup with Esc; suppresses
    /// re-activation until the filter text changes (Backspace) or the
    /// input is cleared.
    pub slash_dismissed: bool,

    // ── Help overlay ──
    /// `true` while the help overlay is shown; all keys are swallowed
    /// except the dismiss keys.
    pub show_help: bool,

    // ── Conversation auto-save ──
    /// Thread name for auto-save, set from the first user message.
    /// `None` until the first message after app start or `/new`.
    pub conversation_title: Option<String>,

    // ── Intervention UI state ──
    /// Index of the currently highlighted option while an intervention
    /// prompt is pending. `None` when no intervention is active.
    pub intervene_selection: Option<usize>,
    /// `true` when the user is typing custom text for an "Other…" option.
    pub intervene_text_mode: bool,
    /// Saved input buffer before entering custom-text mode, restored on
    /// submit or cancel.
    pub intervene_saved_input: String,
    /// Saved cursor position before entering custom-text mode.
    pub intervene_saved_cursor: usize,

    // ── Exit signal ──
    pub should_quit: bool,

    // ── Text selection ──
    /// Current mouse-drag text selection, if any.
    pub selection: Option<SelectionState>,
    /// Cached chat area rect — set each frame in [`super::ui::draw`].
    pub chat_area: Rect,
    /// Total number of wrapped rendered lines — set each frame.
    pub total_rendered_lines: usize,
    /// Number of visible rows inside the chat border — set each frame.
    pub visible_chat_height: usize,
    /// Plain text of each wrapped chat line — rebuilt each frame by
    /// [`super::ui::draw_chat`]. Selection copy slices these strings by
    /// display column, so what lands on the clipboard is exactly what was
    /// highlighted on screen.
    pub rendered_chat_lines: Vec<String>,

    // ── Persistence ──
    pub persistence_config: PersistenceConfig,

    // ── Observability ──
    /// Shared trace store — written by [`ObservabilityHook`], read by TUI status bar.
    pub trace_store: Arc<TraceStore>,

    // ── Plan mode ──
    /// Shared plan-mode toggle between TUI and [`PlanModeHook`].
    pub plan_mode: Arc<PlanModeState>,
    /// Directory where approved plans are archived (`.loomis/plan/`).
    pub plan_dir: PathBuf,

    // ── Skills ──
    /// Discovered skills — read-only after startup, used by `/skill` command.
    pub skill_registry: Arc<SkillRegistry>,
    /// Currently active skills — written by `/skill` and [`SkillTool`].
    pub active_skills: skills::ActiveSkills,

    // ── Sandbox ──
    /// Shell-command policy for user `!command` invocations — the same
    /// rules the [`SandboxHook`] applies to LLM-initiated shell calls.
    pub shell_filter: sandbox::shell_filter::ShellFilter,
    /// A `!command` awaiting y/n confirmation (policy: RequiresApproval).
    pub pending_shell_confirm: Option<String>,

    // ── Retry ──
    /// The last input submitted to the agent — enables Ctrl+R retry
    /// after a failed run (Nielsen #9: recover from errors).
    pub last_submitted_input: Option<String>,
}

impl App {
    /// Creates a fresh app with a welcome system message.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        model: impl Into<String>,
        memory: SharedMemory,
        tool_names: Vec<String>,
        todos: Arc<RwLock<Vec<TodoItem>>>,
        workspace_root: PathBuf,
        pending_hints: PendingHints,
        persistence_config: PersistenceConfig,
        trace_store: Arc<TraceStore>,
        plan_mode: Arc<PlanModeState>,
        plan_dir: PathBuf,
        skill_registry: Arc<SkillRegistry>,
        active_skills: skills::ActiveSkills,
        shell_filter: sandbox::shell_filter::ShellFilter,
    ) -> Self {
        let model = model.into();
        Self {
            messages: vec![ChatMessage::Welcome {
                model: model.clone(),
                workspace: workspace_root.display().to_string(),
            }],
            line_counts: vec![1],
            input: String::new(),
            input_cursor: 0,
            paste_store: PasteStore::default(),
            input_scroll_offset: 0,
            scroll_offset: 0,
            auto_scroll: true,
            streaming: false,
            run_started_at: None,
            spinner_frame: 0,
            last_spinner_tick: Instant::now(),
            model,
            memory,
            tool_names,
            todos,
            workspace_root,
            pending_hints,
            history: Vec::new(),
            history_index: None,
            draft_input: String::new(),
            thread_picker: None,
            slash_completion: None,
            slash_dismissed: false,
            show_help: false,
            conversation_title: None,
            intervene_selection: None,
            intervene_text_mode: false,
            intervene_saved_input: String::new(),
            intervene_saved_cursor: 0,
            should_quit: false,
            selection: None,
            chat_area: Rect::default(),
            total_rendered_lines: 0,
            visible_chat_height: 0,
            rendered_chat_lines: Vec::new(),
            persistence_config,
            trace_store,
            plan_mode,
            plan_dir,
            skill_registry,
            active_skills,
            shell_filter,
            pending_shell_confirm: None,
            last_submitted_input: None,
        }
    }
}

// ── Trace Sync ────────────────────────────────────────────────────────────────────

// ── Event Application ────────────────────────────────────────────────────────────

impl App {
    /// Streams an [`AgentEvent`] into the display state.
    ///
    /// This is called from the main event loop via `try_recv` — it processes
    /// events faster than the render frame rate, so the display stays current.
    pub fn apply_event(&mut self, event: AgentEvent) {
        match event {
            // ── Run lifecycle ────────────────────────────────────────
            AgentEvent::RunStarted { .. } => {
                self.streaming = true;
                self.run_started_at = Some(Instant::now());
            }

            AgentEvent::RunCompleted { .. } => {
                self.streaming = false;
                self.run_started_at = None;
            }

            AgentEvent::RunFailed { error } => {
                self.selection = None;
                self.run_started_at = None;
                let content = format!(
                    "{}\n\nPress Ctrl+R to retry the last submission.",
                    improve_error_message(&error)
                );
                self.messages.push(ChatMessage::Error {
                    content,
                    timestamp: ChatMessage::now_timestamp(),
                });
            }

            AgentEvent::Cancelled => {
                self.selection = None;
                self.run_started_at = None;
                self.messages.push(ChatMessage::System {
                    content: "[Cancelled]".into(),
                    timestamp: ChatMessage::now_timestamp(),
                });
            }

            // ── LLM output ───────────────────────────────────────────
            AgentEvent::Token(text) => match self.messages.last_mut() {
                Some(ChatMessage::Assistant { content, .. }) => {
                    content.push_str(&text);
                }
                _ => {
                    self.messages.push(ChatMessage::Assistant {
                        content: text,
                        timestamp: ChatMessage::now_timestamp(),
                    });
                }
            },

            AgentEvent::ReasoningToken(text) => match self.messages.last_mut() {
                Some(ChatMessage::Reasoning { content, .. }) => {
                    content.push_str(&text);
                }
                _ => {
                    self.messages.push(ChatMessage::Reasoning {
                        content: text,
                        timestamp: ChatMessage::now_timestamp(),
                    });
                }
            },

            // ── Tool lifecycle ───────────────────────────────────────
            AgentEvent::ToolCallStart { id, name } => {
                self.selection = None;
                self.messages.push(ChatMessage::ToolCall {
                    id,
                    name,
                    args: String::new(),
                    state: ToolCallState::Running,
                    origin: CallOrigin::Llm,
                    progress_lines: Vec::new(),
                    timestamp: ChatMessage::now_timestamp(),
                });
            }

            AgentEvent::ToolCall {
                id,
                name,
                arguments,
                origin,
            } => {
                // Upsert: if ToolCallStart already created a Running entry
                // for this id, update its args; otherwise create one.
                let existing = self.messages.iter_mut().rev().find(|msg| {
                    matches!(msg, ChatMessage::ToolCall { id: mid, state: ToolCallState::Running, .. } if *mid == id)
                });
                match existing {
                    Some(ChatMessage::ToolCall { args, .. }) => {
                        *args = arguments;
                    }
                    _ => {
                        self.messages.push(ChatMessage::ToolCall {
                            id,
                            name,
                            args: arguments,
                            state: ToolCallState::Running,
                            origin,
                            progress_lines: Vec::new(),
                            timestamp: ChatMessage::now_timestamp(),
                        });
                    }
                }
            }

            AgentEvent::ToolSuccessful { id, output, .. } => {
                for msg in self.messages.iter_mut().rev() {
                    if let ChatMessage::ToolCall { id: mid, state, .. } = msg
                        && *mid == id
                    {
                        *state = ToolCallState::Complete(output);
                        break;
                    }
                }
            }

            AgentEvent::ToolRejected {
                id,
                name: _,
                reason,
            } => {
                for msg in self.messages.iter_mut().rev() {
                    if let ChatMessage::ToolCall { id: mid, state, .. } = msg
                        && *mid == id
                    {
                        *state = ToolCallState::Rejected(reason);
                        break;
                    }
                }
            }

            AgentEvent::ToolFailure { id, name: _, error } => {
                for msg in self.messages.iter_mut().rev() {
                    if let ChatMessage::ToolCall { id: mid, state, .. } = msg
                        && *mid == id
                    {
                        *state = ToolCallState::Error(error);
                        break;
                    }
                }
            }

            AgentEvent::ToolProgress { id, message, .. } => {
                for msg in self.messages.iter_mut().rev() {
                    if let ChatMessage::ToolCall {
                        id: mid,
                        state,
                        progress_lines,
                        ..
                    } = msg
                        && *mid == id
                        && matches!(state, ToolCallState::Running)
                    {
                        progress_lines.push(message);
                        break;
                    }
                }
            }

            // ── Intervention ─────────────────────────────────────────
            AgentEvent::InterventionRequired(req) => {
                self.selection = None;
                self.messages.push(ChatMessage::Intervene {
                    request_id: req.request_id,
                    title: req.title,
                    description: req.description,
                    options: req.options,
                    responded: false,
                    chosen: None,
                    custom_text: None,
                    timestamp: ChatMessage::now_timestamp(),
                });
                // Default-select the first option so it renders highlighted
                // immediately, before the user presses any key.
                self.intervene_selection = Some(0);
            }

            // ── Terminal sentinel ────────────────────────────────────
            AgentEvent::Done => {
                self.streaming = false;
                self.run_started_at = None;
            }
        }

        // Auto-scroll to bottom when new content arrives and user hasn't
        // manually scrolled up.
        if self.auto_scroll {
            self.scroll_offset = 0;
        }
    }
}

/// Appends an actionable hint to common error categories so the user
/// knows what to do next (Nielsen #9: help users recognize, diagnose,
/// and recover from errors). Unknown errors pass through unchanged.
fn improve_error_message(error: &str) -> String {
    let lower = error.to_lowercase();
    let hint = if lower.contains("api key")
        || lower.contains("apikey")
        || lower.contains("unauthorized")
        || lower.contains("401")
    {
        Some("Check that DEEPSEEK_API is set correctly in your environment or .env file.")
    } else if lower.contains("rate limit") || lower.contains("429") {
        Some("Rate limited — wait a moment before retrying.")
    } else if lower.contains("timeout") || lower.contains("timed out") {
        Some("The request timed out — check your network connection and try again.")
    } else if lower.contains("connect") || lower.contains("dns") || lower.contains("network") {
        Some("Network error — check your connection and proxy settings.")
    } else {
        None
    };

    match hint {
        Some(h) => format!("{error}\nHint: {h}"),
        None => error.to_string(),
    }
}

// ── Text Selection ────────────────────────────────────────────────────────────────

/// Extracts the substring of `text` covered by the display-column range
/// `[start_col, end_col)`.
///
/// Display columns are terminal cells, not bytes: a CJK character occupies
/// two columns. A character whose cells only *partially* overlap the range
/// is included in full — half a glyph can neither be displayed nor
/// meaningfully copied.
fn slice_by_display_columns(text: &str, start_col: usize, end_col: usize) -> String {
    if start_col >= end_col || text.is_empty() {
        return String::new();
    }

    let mut selected_byte_start: Option<usize> = None;
    let mut selected_byte_end: usize = 0;
    let mut column: usize = 0;

    for (byte_index, ch) in text.char_indices() {
        let char_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        let char_end_column = column + char_width;
        // The character is selected when its cells overlap the range.
        if char_end_column > start_col && column < end_col {
            if selected_byte_start.is_none() {
                selected_byte_start = Some(byte_index);
            }
            selected_byte_end = byte_index + ch.len_utf8();
        }
        column = char_end_column;
    }

    match selected_byte_start {
        Some(start) => text[start..selected_byte_end].to_string(),
        None => String::new(),
    }
}

impl App {
    /// Extracts exactly the text covered by the current selection.
    ///
    /// Slices the per-frame [`App::rendered_chat_lines`] cache by display
    /// column, so the clipboard receives what was visibly highlighted —
    /// including the timestamp and `>` prefixes the user saw selected.
    pub fn get_selection_text(&self) -> String {
        let sel = match &self.selection {
            Some(s) if !s.dragging => s,
            _ => return String::new(),
        };
        let (start_line, start_col, end_line, end_col) = sel.ordered_bounds();

        let lines = &self.rendered_chat_lines;
        if start_line >= lines.len() {
            return String::new();
        }
        // The selection may point past the end after new content arrived.
        let last_line = end_line.min(lines.len() - 1);

        let mut selected_parts: Vec<String> = Vec::new();
        for (line_index, line_text) in lines
            .iter()
            .enumerate()
            .skip(start_line)
            .take(last_line - start_line + 1)
        {
            let part = if line_index == start_line && line_index == last_line {
                // Single-line selection: slice out the column range.
                slice_by_display_columns(line_text, start_col, end_col)
            } else if line_index == start_line {
                // First line of a multi-line selection: start column → EOL.
                let line_width = UnicodeWidthStr::width(line_text.as_str());
                slice_by_display_columns(line_text, start_col, line_width)
            } else if line_index == last_line {
                // Last line: BOL → end column.
                slice_by_display_columns(line_text, 0, end_col)
            } else {
                // Middle lines are selected in full.
                line_text.clone()
            };
            selected_parts.push(part);
        }

        selected_parts.join("\n")
    }

    /// Copies the finalized selection to the system clipboard. No-op when
    /// there is no finalized selection or it holds no text.
    ///
    /// Shared by the copy shortcut (`Cmd+C` on macOS, `Ctrl+C` elsewhere)
    /// and — on macOS — by selection completion, where the terminal
    /// intercepts the copy key and the text must be copied the moment the
    /// selection is made (the native "select = copy" behaviour).
    pub fn copy_selection_to_clipboard(&mut self) {
        // `get_selection_text` returns an empty string when there is no
        // finalized selection or it covers no text — both are no-ops.
        let text = self.get_selection_text();
        if text.is_empty() {
            return;
        }
        match arboard::Clipboard::new() {
            Ok(mut clipboard) => {
                let _ = clipboard.set_text(text);
            }
            Err(e) => {
                self.messages.push(ChatMessage::System {
                    content: format!("Clipboard error: {e}"),
                    timestamp: ChatMessage::now_timestamp(),
                });
            }
        }
    }

    /// Clears the current selection.
    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Checks whether terminal coordinates (col, row) fall inside the chat
    /// inner area (i.e. inside the border of the chat block).
    fn is_in_chat_area(&self, col: u16, row: u16) -> bool {
        let block = ratatui::widgets::Block::default().borders(ratatui::widgets::Borders::ALL);
        let inner = block.inner(self.chat_area);
        col >= inner.x
            && col < inner.x + inner.width
            && row >= inner.y
            && row < inner.y + inner.height
    }

    /// Converts terminal screen coordinates to a `(line, display_column)`
    /// position in the wrapped `rendered_chat_lines` vec.
    fn screen_to_position(&self, col: u16, row: u16) -> (usize, usize) {
        let block = ratatui::widgets::Block::default().borders(ratatui::widgets::Borders::ALL);
        let inner = block.inner(self.chat_area);
        let visible_row = (row.saturating_sub(inner.y)) as usize;
        let max_scroll = self
            .total_rendered_lines
            .saturating_sub(self.visible_chat_height);
        let scroll = max_scroll
            .saturating_sub(self.scroll_offset)
            .min(max_scroll);
        let line_index = scroll + visible_row;
        let display_column = col.saturating_sub(inner.x) as usize;
        (line_index, display_column)
    }

    /// Handles a crossterm mouse event for text selection (Left click/drag/up)
    /// and scroll wheel. Called from the event loop.
    pub fn handle_mouse_event(&mut self, event: &crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};

        let col = event.column;
        let row = event.row;

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.is_in_chat_area(col, row) {
                    let (line_index, display_column) = self.screen_to_position(col, row);
                    self.selection = Some(SelectionState {
                        start_line: line_index,
                        start_col: display_column,
                        end_line: line_index,
                        end_col: display_column,
                        dragging: true,
                    });
                } else {
                    // Click outside chat area clears selection.
                    self.selection = None;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let (line_index, display_column) = self.screen_to_position(col, row);
                if let Some(ref mut sel) = self.selection
                    && sel.dragging
                {
                    sel.end_line = line_index;
                    sel.end_col = display_column;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let is_completed_selection = if let Some(ref mut sel) = self.selection {
                    sel.dragging = false;
                    // A click without movement is not a selection — dropping
                    // it keeps an accidental click from hijacking the next
                    // Ctrl+C (which would copy one line instead of
                    // cancelling the stream).
                    sel.start_line != sel.end_line || sel.start_col != sel.end_col
                } else {
                    false
                };
                if is_completed_selection && cfg!(target_os = "macos") {
                    // macOS terminals swallow the Cmd+C menu shortcut at
                    // the terminal level, so the app never receives the
                    // copy key. Copy the selection as it completes — the
                    // native terminal "select = copy" behaviour — so
                    // selected text reaches the clipboard regardless.
                    self.copy_selection_to_clipboard();
                } else if !is_completed_selection && self.selection.is_some() {
                    self.selection = None;
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                // Windows terminal convention: right-click pastes. Only
                // terminals that forward the right button to the
                // application reach this arm — terminals that inject the
                // clipboard themselves (legacy conhost, Windows Terminal)
                // deliver the paste as a key-event burst instead, which
                // the event loop coalesces.
                self.paste_from_clipboard();
            }
            MouseEventKind::ScrollUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(4);
                self.auto_scroll = false;
            }
            MouseEventKind::ScrollDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(4);
                if self.scroll_offset == 0 {
                    self.auto_scroll = true;
                }
            }
            _ => {}
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::messages::TuiCommand;
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use engine::CallOrigin;

    fn ts() -> String {
        "00:00:00".into()
    }

    fn make_app() -> App {
        let memory = std::sync::Arc::new(std::sync::RwLock::new(memory::Memory::new()));
        let pending_hints = PendingHints::default();
        let todos = Arc::new(RwLock::new(Vec::<TodoItem>::new()));
        let trace_store = Arc::new(TraceStore::new());
        let plan_mode = Arc::new(PlanModeState::default());
        let plan_dir = PathBuf::from(".loomis/plan");
        let skill_registry = Arc::new(SkillRegistry::empty());
        let active_skills = Arc::new(RwLock::new(std::collections::HashMap::new()));
        let shell_filter =
            sandbox::shell_filter::ShellFilter::from_config(&sandbox::SandboxConfig::default());
        App::new(
            "test-model",
            memory,
            vec!["echo".into(), "ls".into()],
            todos,
            PathBuf::from("."),
            pending_hints,
            PersistenceConfig::default(),
            trace_store,
            plan_mode,
            plan_dir,
            skill_registry,
            active_skills,
            shell_filter,
        )
    }

    // ── apply_event ─────────────────────────────────────────────

    #[test]
    fn test_spinner_run_lifecycle() {
        let mut app = make_app();
        assert!(app.run_started_at.is_none());

        app.apply_event(AgentEvent::RunStarted {
            session_id: "s1".into(),
            user_input: "hi".into(),
        });
        assert!(app.run_started_at.is_some());
        assert!(app.streaming);

        app.apply_event(AgentEvent::RunCompleted {
            answer: "done".into(),
        });
        assert!(app.run_started_at.is_none());

        // Failed run also clears the timer.
        app.apply_event(AgentEvent::RunStarted {
            session_id: "s2".into(),
            user_input: "hi".into(),
        });
        assert!(app.run_started_at.is_some());
        app.apply_event(AgentEvent::RunFailed {
            error: "boom".into(),
        });
        assert!(app.run_started_at.is_none());

        // Done sentinel clears it too.
        app.apply_event(AgentEvent::RunStarted {
            session_id: "s3".into(),
            user_input: "hi".into(),
        });
        app.apply_event(AgentEvent::Done);
        assert!(app.run_started_at.is_none());
        assert!(!app.streaming);
    }

    #[test]
    fn test_apply_token_creates_assistant() {
        let mut app = make_app();
        // clear the welcome message for clean test state
        app.messages.clear();
        app.apply_event(AgentEvent::Token("Hello".into()));
        assert_eq!(app.messages.len(), 1);
        match &app.messages[0] {
            ChatMessage::Assistant { content, .. } => assert_eq!(content, "Hello"),
            other => panic!("expected Assistant, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_token_appends_to_last_assistant() {
        let mut app = make_app();
        app.messages.clear();
        app.apply_event(AgentEvent::Token("Hel".into()));
        app.apply_event(AgentEvent::Token("lo".into()));
        assert_eq!(app.messages.len(), 1);
        match &app.messages[0] {
            ChatMessage::Assistant { content, .. } => assert_eq!(content, "Hello"),
            other => panic!("expected Assistant, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_token_new_assistant_after_tool_call() {
        let mut app = make_app();
        app.messages.clear();
        app.apply_event(AgentEvent::Token("Before".into()));
        app.apply_event(AgentEvent::ToolCall {
            id: "t1".into(),
            name: "echo".into(),
            arguments: r#"{"x":1}"#.into(),
            origin: CallOrigin::Llm,
        });
        app.apply_event(AgentEvent::ToolSuccessful {
            id: "t1".into(),
            name: "echo".into(),
            output: "ok".into(),
        });
        // New token after tool result creates a fresh Assistant message
        app.apply_event(AgentEvent::Token("After".into()));

        assert_eq!(app.messages.len(), 3); // Before, ToolCall, After
        match &app.messages[0] {
            ChatMessage::Assistant { content, .. } => assert_eq!(content, "Before"),
            other => panic!("expected Assistant, got {other:?}"),
        }
        match &app.messages[2] {
            ChatMessage::Assistant { content, .. } => assert_eq!(content, "After"),
            other => panic!("expected Assistant, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_done_sets_streaming_false() {
        let mut app = make_app();
        app.streaming = true;
        app.apply_event(AgentEvent::Done);
        assert!(!app.streaming);
    }

    #[test]
    fn test_apply_tool_call_lifecycle() {
        let mut app = make_app();
        app.messages.clear();
        app.apply_event(AgentEvent::ToolCall {
            id: "abc".into(),
            name: "ls".into(),
            arguments: r#"{"path":"."}"#.into(),
            origin: CallOrigin::Llm,
        });
        app.apply_event(AgentEvent::ToolSuccessful {
            id: "abc".into(),
            name: "ls".into(),
            output: "src/\nCargo.toml".into(),
        });

        assert_eq!(app.messages.len(), 1);
        match &app.messages[0] {
            ChatMessage::ToolCall {
                id,
                name,
                args,
                state,
                ..
            } => {
                assert_eq!(id, "abc");
                assert_eq!(name, "ls");
                assert_eq!(args, r#"{"path":"."}"#);
                match state {
                    ToolCallState::Complete(out) => assert_eq!(out, "src/\nCargo.toml"),
                    ToolCallState::Running => panic!("expected Complete"),
                    ToolCallState::Error(_) => panic!("expected Complete, got Error"),
                    ToolCallState::Rejected(_) => panic!("expected Complete, got Rejected"),
                }
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_reasoning_token() {
        let mut app = make_app();
        app.messages.clear();
        app.apply_event(AgentEvent::ReasoningToken("Hmm, ".into()));
        app.apply_event(AgentEvent::ReasoningToken("let me think...".into()));
        assert_eq!(app.messages.len(), 1);
        match &app.messages[0] {
            ChatMessage::Reasoning { content, .. } => {
                assert_eq!(content, "Hmm, let me think...");
            }
            other => panic!("expected Reasoning, got {other:?}"),
        }
    }

    // ── handle_key ──────────────────────────────────────────────

    #[test]
    fn test_slash_exit_returns_exit_command() {
        let mut app = make_app();
        app.input = "/exit".into();
        app.input_cursor = 5;

        let result = submit_via_enter(&mut app);
        assert!(matches!(result, Some(TuiCommand::Exit)));
        assert!(app.should_quit);
    }

    #[test]
    fn test_slash_new_returns_clear_command() {
        let mut app = make_app();
        app.messages.push(ChatMessage::User {
            content: "old".into(),
            timestamp: ts(),
        });
        app.input = "/new".into();
        app.input_cursor = 4;

        let result = submit_via_enter(&mut app);
        assert!(matches!(result, Some(TuiCommand::ClearConversation)));
        // Local messages cleared, replaced with system confirmation
        assert_eq!(app.messages.len(), 1);
        match &app.messages[0] {
            ChatMessage::System { content, .. } => {
                assert!(content.contains("New conversation"));
            }
            other => panic!("expected System, got {other:?}"),
        }
    }

    #[test]
    fn test_slash_stats_returns_none() {
        let mut app = make_app();
        app.input = "/stats".into();
        app.input_cursor = 6;

        let result = submit_via_enter(&mut app);
        assert!(result.is_none()); // handled locally
        // welcome message + stats response
        assert_eq!(app.messages.len(), 2);
        match &app.messages[1] {
            ChatMessage::System { content, .. } => {
                assert!(content.contains("Messages"));
            }
            other => panic!("expected System, got {other:?}"),
        }
    }

    #[test]
    fn test_slash_plan_with_text_activates_and_runs_agent() {
        let mut app = make_app();
        app.input = "/plan 帮我出份计划".into();
        app.input_cursor = app.input.len();

        let result = submit_via_enter(&mut app);
        // Trailing text must still be recognized as the /plan command: plan
        // mode is activated and the text is forwarded to the agent.
        assert!(
            app.plan_mode
                .active
                .load(std::sync::atomic::Ordering::SeqCst)
        );
        match result {
            Some(TuiCommand::RunAgent(msg)) => assert_eq!(msg, "帮我出份计划"),
            other => panic!("expected RunAgent, got {other:?}"),
        }
    }

    #[test]
    fn test_slash_plan_bare_still_toggles() {
        let mut app = make_app();
        app.input = "/plan".into();
        app.input_cursor = 5;
        assert!(submit_via_enter(&mut app).is_none());
        assert!(
            app.plan_mode
                .active
                .load(std::sync::atomic::Ordering::SeqCst)
        );

        app.input = "/plan".into();
        app.input_cursor = 5;
        assert!(submit_via_enter(&mut app).is_none());
        assert!(
            !app.plan_mode
                .active
                .load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    #[test]
    fn test_slash_init_with_text_appends_instruction() {
        let mut app = make_app();
        app.input = "/init 记得带上日志".into();
        app.input_cursor = app.input.len();

        let result = submit_via_enter(&mut app);
        match result {
            Some(TuiCommand::RunAgent(prompt)) => {
                assert!(prompt.contains("INIT MODE"));
                assert!(prompt.contains("记得带上日志"));
            }
            other => panic!("expected RunAgent, got {other:?}"),
        }
    }

    #[test]
    fn test_normal_message_returns_run_agent() {
        let mut app = make_app();
        app.input = "hello".into();
        app.input_cursor = 5;

        let result = submit_via_enter(&mut app);
        assert!(matches!(result, Some(TuiCommand::RunAgent(msg)) if msg == "hello"));
        assert!(app.streaming);
        // Input cleared
        assert!(app.input.is_empty());
        assert_eq!(app.input_cursor, 0);
        // welcome message + user message
        assert_eq!(app.messages.len(), 2);
        match &app.messages[1] {
            ChatMessage::User { content, .. } => assert_eq!(content, "hello"),
            other => panic!("expected User, got {other:?}"),
        }
    }

    #[test]
    fn test_ctrl_c_while_streaming_cancels() {
        let mut app = make_app();
        app.streaming = true;
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = app.handle_key(key);
        assert!(matches!(result, Some(TuiCommand::CancelGeneration)));
        assert!(!app.streaming);
    }

    #[test]
    fn test_ctrl_c_while_idle_shows_hint_instead_of_exiting() {
        let mut app = make_app();
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = app.handle_key(key);
        // Nielsen #4: Ctrl+C means copy/cancel everywhere — never "quit".
        assert!(result.is_none());
        assert!(!app.should_quit);
        match app.messages.last() {
            Some(ChatMessage::System { content, .. }) => {
                assert!(content.contains("Ctrl+D"));
            }
            other => panic!("expected System hint, got {other:?}"),
        }
    }

    #[test]
    fn test_ctrl_d_while_idle_exits() {
        let mut app = make_app();
        let key = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        let result = app.handle_key(key);
        assert!(matches!(result, Some(TuiCommand::Exit)));
        assert!(app.should_quit);
    }

    #[test]
    fn test_esc_while_streaming_cancels() {
        let mut app = make_app();
        app.streaming = true;
        let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let result = app.handle_key(key);
        assert!(matches!(result, Some(TuiCommand::CancelGeneration)));
        assert!(!app.streaming);
    }

    #[test]
    fn test_input_scroll_offset_follows_cursor() {
        let mut app = make_app();
        // Cursor rows within the visible window → no scrolling.
        app.update_input_scroll_offset(0, 15);
        assert_eq!(app.input_scroll_offset, 0);
        app.update_input_scroll_offset(14, 15);
        assert_eq!(app.input_scroll_offset, 0);
        // Cursor past the bottom → scroll down to keep it visible.
        app.update_input_scroll_offset(20, 15);
        assert_eq!(app.input_scroll_offset, 6);
        // Cursor above the window → scroll back up.
        app.update_input_scroll_offset(3, 15);
        assert_eq!(app.input_scroll_offset, 3);
    }

    // ── Slash completion ────────────────────────────────────────

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    #[test]
    fn test_slash_completion_activates_on_slash() {
        let mut app = make_app();
        type_str(&mut app, "/");
        let sc = app.slash_completion.as_ref().expect("popup should open");
        assert_eq!(sc.matches.len(), crate::tui::messages::SLASH_COMMANDS.len());
        assert_eq!(sc.selected, 0);
    }

    #[test]
    fn test_slash_completion_filters_by_prefix() {
        let mut app = make_app();
        type_str(&mut app, "/s");
        let sc = app
            .slash_completion
            .as_ref()
            .expect("popup should stay open");
        let names: Vec<&str> = sc.matches.iter().map(|c| c.name).collect();
        assert!(names.contains(&"save"));
        assert!(names.contains(&"stats"));
        assert!(names.contains(&"skill"));
        assert!(!names.contains(&"exit"));
    }

    #[test]
    fn test_slash_completion_tab_accepts() {
        let mut app = make_app();
        type_str(&mut app, "/exi");
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input, "/exit ");
        assert!(app.slash_completion.is_none());
    }

    #[test]
    fn test_slash_completion_arrow_navigation() {
        let mut app = make_app();
        type_str(&mut app, "/");
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.slash_completion.as_ref().unwrap().selected, 1);
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.slash_completion.as_ref().unwrap().selected, 0);
        // Up at the top stays put (no wrap).
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.slash_completion.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn test_slash_completion_esc_dismisses_until_text_changes() {
        let mut app = make_app();
        type_str(&mut app, "/");
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.slash_completion.is_none());
        assert!(app.slash_dismissed);
        // Typing more does not reopen the popup.
        type_str(&mut app, "ex");
        assert!(app.slash_completion.is_none());
        // Backspace signals a fresh session — typing reopens it.
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(!app.slash_dismissed);
        type_str(&mut app, "x");
        assert!(app.slash_completion.is_some());
    }

    #[test]
    fn test_slash_completion_enter_submits_argless_command() {
        let mut app = make_app();
        type_str(&mut app, "/exi");
        let result = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(result, Some(TuiCommand::Exit)));
        assert!(app.should_quit);
    }

    #[test]
    fn test_slash_completion_enter_only_accepts_command_with_args() {
        let mut app = make_app();
        type_str(&mut app, "/sa");
        let result = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        // `/save <name>` requires an argument — accepted, not submitted.
        assert!(result.is_none());
        assert_eq!(app.input, "/save ");
        assert!(app.slash_completion.is_none());
    }

    #[test]
    fn test_slash_completion_closes_on_no_match() {
        let mut app = make_app();
        type_str(&mut app, "/zzz");
        assert!(app.slash_completion.is_none());
    }

    #[test]
    fn test_slash_completion_not_in_streaming() {
        let mut app = make_app();
        app.streaming = true;
        type_str(&mut app, "/st");
        assert!(app.slash_completion.is_none());
    }

    // ── Help overlay ────────────────────────────────────────────

    #[test]
    fn test_help_opens_on_question_mark_when_idle() {
        let mut app = make_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        assert!(app.show_help);
        assert!(app.input.is_empty()); // '?' was not inserted
    }

    #[test]
    fn test_help_question_mark_inserts_when_input_nonempty() {
        let mut app = make_app();
        type_str(&mut app, "what");
        app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        assert!(!app.show_help);
        assert_eq!(app.input, "what?");
    }

    #[test]
    fn test_help_not_opened_while_streaming() {
        let mut app = make_app();
        app.streaming = true;
        app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        assert!(!app.show_help);
        assert_eq!(app.input, "?");
    }

    #[test]
    fn test_help_dismiss_and_swallow() {
        let mut app = make_app();
        app.show_help = true;
        // Unrelated keys are swallowed, help stays open.
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(app.show_help);
        assert!(app.input.is_empty());
        // Esc closes.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.show_help);
    }

    #[test]
    fn test_help_f1_toggles() {
        let mut app = make_app();
        app.handle_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        assert!(app.show_help);
        app.handle_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        assert!(!app.show_help);
    }

    // ── Shell confirmation (ShellFilter integration) ────────────

    #[test]
    fn test_bang_auto_approved_runs_directly() {
        let mut app = make_app();
        app.input = "!echo hello".into();
        app.input_cursor = app.input.len();
        let result = submit_via_enter(&mut app);
        assert!(matches!(result, Some(TuiCommand::RunShell(cmd)) if cmd == "echo hello"));
        assert!(app.pending_shell_confirm.is_none());
    }

    #[test]
    fn test_bang_blocked_is_rejected_with_reason() {
        let mut app = make_app();
        app.input = "!sudo rm -rf /".into();
        app.input_cursor = app.input.len();
        let result = submit_via_enter(&mut app);
        assert!(result.is_none());
        assert!(app.pending_shell_confirm.is_none());
        match app.messages.last() {
            Some(ChatMessage::Error { content, .. }) => {
                assert!(content.contains("Blocked by sandbox policy"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn test_bang_requires_approval_enters_confirm_mode() {
        let mut app = make_app();
        app.input = "!curl https://example.com".into();
        app.input_cursor = app.input.len();
        let result = submit_via_enter(&mut app);
        assert!(result.is_none());
        assert_eq!(
            app.pending_shell_confirm.as_deref(),
            Some("curl https://example.com")
        );
    }

    #[test]
    fn test_shell_confirm_y_executes() {
        let mut app = make_app();
        app.pending_shell_confirm = Some("curl https://example.com".into());
        let result = app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(
            matches!(result, Some(TuiCommand::RunShell(cmd)) if cmd == "curl https://example.com")
        );
        assert!(app.pending_shell_confirm.is_none());
    }

    #[test]
    fn test_shell_confirm_n_cancels() {
        let mut app = make_app();
        app.pending_shell_confirm = Some("curl https://example.com".into());
        let result = app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(result.is_none());
        assert!(app.pending_shell_confirm.is_none());
        match app.messages.last() {
            Some(ChatMessage::System { content, .. }) => {
                assert!(content.contains("cancelled"));
            }
            other => panic!("expected System, got {other:?}"),
        }
    }

    #[test]
    fn test_shell_confirm_swallows_other_keys() {
        let mut app = make_app();
        app.pending_shell_confirm = Some("curl https://example.com".into());
        let result = app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(result.is_none());
        assert!(app.pending_shell_confirm.is_some()); // still pending
    }

    // ── Ctrl+R retry ────────────────────────────────────────────

    #[test]
    fn test_ctrl_r_retries_last_submission() {
        let mut app = make_app();
        app.input = "hello agent".into();
        app.input_cursor = app.input.len();
        let result = submit_via_enter(&mut app);
        assert!(matches!(result, Some(TuiCommand::RunAgent(_))));
        assert_eq!(app.last_submitted_input.as_deref(), Some("hello agent"));

        // Simulate the run ending (failed), then retry.
        app.streaming = false;
        let msg_count_before = app.messages.len();
        let result = app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(matches!(result, Some(TuiCommand::RunAgent(m)) if m == "hello agent"));
        assert!(app.streaming);
        assert_eq!(app.messages.len(), msg_count_before + 1); // user msg re-shown
    }

    #[test]
    fn test_ctrl_r_without_last_submission_is_noop() {
        let mut app = make_app();
        let result = app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(result.is_none());
        assert!(!app.streaming);
    }

    #[test]
    fn test_ctrl_r_ignored_while_streaming() {
        let mut app = make_app();
        app.last_submitted_input = Some("hello".into());
        app.streaming = true;
        let result = app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(result.is_none());
    }

    // ── Error message hints ─────────────────────────────────────

    #[test]
    fn test_improve_error_message_api_key() {
        let out = improve_error_message("HTTP 401 Unauthorized");
        assert!(out.contains("DEEPSEEK_API"));
    }

    #[test]
    fn test_improve_error_message_timeout() {
        let out = improve_error_message("request timed out after 30s");
        assert!(out.contains("network"));
    }

    #[test]
    fn test_improve_error_message_rate_limit() {
        let out = improve_error_message("429 Too Many Requests: rate limit exceeded");
        assert!(out.contains("wait"));
    }

    #[test]
    fn test_improve_error_message_passthrough() {
        let out = improve_error_message("something completely unexpected");
        assert_eq!(out, "something completely unexpected");
    }

    #[test]
    fn test_run_failed_includes_retry_hint() {
        let mut app = make_app();
        app.apply_event(AgentEvent::RunFailed {
            error: "boom".into(),
        });
        match app.messages.last() {
            Some(ChatMessage::Error { content, .. }) => {
                assert!(content.contains("Ctrl+R"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn test_page_up_increases_scroll_offset() {
        let mut app = make_app();
        app.scroll_offset = 0;
        app.auto_scroll = true;
        let key = KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE);
        app.handle_key(key);
        assert_eq!(app.scroll_offset, 8);
        assert!(!app.auto_scroll);
    }

    #[test]
    fn test_page_down_decreases_scroll_offset() {
        let mut app = make_app();
        app.scroll_offset = 10;
        app.auto_scroll = false;
        let key = KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE);
        app.handle_key(key);
        assert_eq!(app.scroll_offset, 2);
        assert!(!app.auto_scroll);
    }

    #[test]
    fn test_page_down_to_zero_reenables_autoscroll() {
        let mut app = make_app();
        app.scroll_offset = 4;
        app.auto_scroll = false;
        let key = KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE);
        app.handle_key(key);
        assert_eq!(app.scroll_offset, 0);
        assert!(app.auto_scroll);
    }

    #[test]
    fn test_character_insertion() {
        let mut app = make_app();
        let key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        app.handle_key(key);
        assert_eq!(app.input, "x");
        assert_eq!(app.input_cursor, 1);
    }

    #[test]
    fn test_backspace_deletes() {
        let mut app = make_app();
        app.input = "ab".into();
        app.input_cursor = 1;
        let key = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        app.handle_key(key);
        assert_eq!(app.input, "b");
        assert_eq!(app.input_cursor, 0);
    }

    #[test]
    fn test_left_right_movement() {
        let mut app = make_app();
        app.input = "abc".into();
        app.input_cursor = 1;

        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.input_cursor, 2);

        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.input_cursor, 1);
    }

    // ── Bang command tests ──────────────────────────────────────

    #[test]
    fn test_bang_command_returns_run_shell() {
        let mut app = make_app();
        app.input = "!echo hello".into();
        app.input_cursor = 11;

        let result = submit_via_enter(&mut app);
        assert!(
            matches!(result, Some(TuiCommand::RunShell(ref cmd)) if cmd == "echo hello"),
            "expected RunShell(\"echo hello\"), got {result:?}"
        );
        assert!(!app.streaming);
        assert!(app.input.is_empty());
        assert_eq!(app.input_cursor, 0);
    }

    #[test]
    fn test_apply_shell_output_creates_message() {
        let mut app = make_app();
        app.messages.clear();
        // User !command now uses unified ToolCall with origin: User
        app.apply_event(AgentEvent::ToolCall {
            id: "shell-1".into(),
            name: "shell".into(),
            arguments: "echo test".into(),
            origin: CallOrigin::User,
        });
        app.apply_event(AgentEvent::ToolSuccessful {
            id: "shell-1".into(),
            name: "shell".into(),
            output: "test".into(),
        });
        assert_eq!(app.messages.len(), 1);
        match &app.messages[0] {
            ChatMessage::ToolCall {
                name,
                args,
                state,
                origin,
                ..
            } => {
                assert_eq!(name, "shell");
                assert_eq!(args, "echo test");
                assert!(matches!(origin, CallOrigin::User));
                match state {
                    ToolCallState::Complete(output) => assert!(output.contains("test")),
                    ToolCallState::Running => panic!("expected Complete"),
                    ToolCallState::Error(_) => panic!("expected Complete, got Error"),
                    ToolCallState::Rejected(_) => panic!("expected Complete, got Rejected"),
                }
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn test_bang_empty_command_shows_help() {
        let mut app = make_app();
        app.messages.clear();
        app.input = "!".into();
        app.input_cursor = 1;

        let result = submit_via_enter(&mut app);
        assert!(result.is_none());
        assert_eq!(app.messages.len(), 1);
        match &app.messages[0] {
            ChatMessage::System { content, .. } => {
                assert!(content.contains("Usage"), "got: {content}");
            }
            other => panic!("expected System, got {other:?}"),
        }
    }

    #[test]
    fn test_double_bang_not_treated_as_command() {
        let mut app = make_app();
        app.messages.clear();
        app.input = "!!echo".into();
        app.input_cursor = 6;

        let result = submit_via_enter(&mut app);
        // !! should be treated as a normal message, triggering RunAgent
        assert!(matches!(result, Some(TuiCommand::RunAgent(_))));
    }

    #[test]
    fn test_bang_command_whitespace_only() {
        let mut app = make_app();
        app.messages.clear();
        app.input = "!   ".into();
        app.input_cursor = 4;

        let result = submit_via_enter(&mut app);
        assert!(result.is_none());
        // Empty command after trimming should show usage hint
        match &app.messages[0] {
            ChatMessage::System { content, .. } => {
                assert!(content.contains("Usage"), "got: {content}");
            }
            other => panic!("expected System, got {other:?}"),
        }
    }

    // ── slice_by_display_columns ────────────────────────────────

    #[test]
    fn test_slice_by_display_columns_ascii() {
        assert_eq!(slice_by_display_columns("hello world", 0, 5), "hello");
        assert_eq!(slice_by_display_columns("hello world", 6, 11), "world");
    }

    #[test]
    fn test_slice_by_display_columns_cjk_aligned() {
        // Each CJK char occupies 2 columns: 你[0,2) 好[2,4) 世[4,6) 界[6,8).
        assert_eq!(slice_by_display_columns("你好世界", 2, 6), "好世");
    }

    #[test]
    fn test_slice_by_display_columns_cjk_partial_overlap_keeps_whole_char() {
        // [1,4) clips 你's right cell and 好's left cell — both chars are
        // still copied in full.
        assert_eq!(slice_by_display_columns("你好世界", 1, 4), "你好");
    }

    #[test]
    fn test_slice_by_display_columns_out_of_bounds() {
        assert_eq!(slice_by_display_columns("abc", 5, 9), "");
        assert_eq!(slice_by_display_columns("abc", 2, 99), "c");
        assert_eq!(slice_by_display_columns("abc", 3, 3), "");
        assert_eq!(slice_by_display_columns("", 0, 4), "");
    }

    // ── get_selection_text ──────────────────────────────────────

    /// Builds an app whose chat area renders `lines`, with the chat block
    /// occupying (0,0)-(40,12) so the inner text area starts at (1,1).
    fn app_with_chat_lines(lines: &[&str]) -> App {
        let mut app = make_app();
        app.rendered_chat_lines = lines.iter().map(|line| line.to_string()).collect();
        app.total_rendered_lines = lines.len();
        app.visible_chat_height = 10;
        app.chat_area = Rect::new(0, 0, 40, 12);
        app
    }

    #[test]
    fn test_get_selection_text_single_line_column_range() {
        let mut app = app_with_chat_lines(&["10:00:00 > hello world"]);
        // Column 11 is the 'h' in "hello" (8 timestamp + space + "> ").
        app.selection = Some(SelectionState {
            start_line: 0,
            start_col: 11,
            end_line: 0,
            end_col: 16,
            dragging: false,
        });
        assert_eq!(app.get_selection_text(), "hello");
    }

    #[test]
    fn test_get_selection_text_multi_line_truncates_first_and_last() {
        let mut app = app_with_chat_lines(&["first line", "middle line", "last line"]);
        app.selection = Some(SelectionState {
            start_line: 0,
            start_col: 6,
            end_line: 2,
            end_col: 4,
            dragging: false,
        });
        assert_eq!(app.get_selection_text(), "line\nmiddle line\nlast");
    }

    #[test]
    fn test_get_selection_text_upward_drag_same_result() {
        let mut app = app_with_chat_lines(&["first line", "middle line", "last line"]);
        // Dragging from bottom to top must select the same text.
        app.selection = Some(SelectionState {
            start_line: 2,
            start_col: 4,
            end_line: 0,
            end_col: 6,
            dragging: false,
        });
        assert_eq!(app.get_selection_text(), "line\nmiddle line\nlast");
    }

    #[test]
    fn test_get_selection_text_empty_without_finalized_selection() {
        let mut app = app_with_chat_lines(&["some text"]);
        // No selection at all.
        assert_eq!(app.get_selection_text(), "");
        // Still dragging — copy must wait for mouse-up.
        app.selection = Some(SelectionState {
            start_line: 0,
            start_col: 0,
            end_line: 0,
            end_col: 4,
            dragging: true,
        });
        assert_eq!(app.get_selection_text(), "");
    }

    // ── Mouse-driven selection ──────────────────────────────────

    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    /// Builds a mouse event at `(col, row)` with no modifiers held.
    fn mouse_event(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn test_mouse_click_without_drag_clears_selection() {
        let mut app = app_with_chat_lines(&["a", "b", "c"]);
        app.handle_mouse_event(&mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 5));
        assert!(app.selection.is_some(), "mouse-down starts a selection");

        app.handle_mouse_event(&mouse_event(MouseEventKind::Up(MouseButton::Left), 5, 5));
        assert!(
            app.selection.is_none(),
            "click without movement must not leave a selection"
        );
    }

    #[test]
    fn test_mouse_drag_records_line_and_column() {
        let mut app = app_with_chat_lines(&["a", "b", "c", "d", "e", "f", "g", "h"]);
        app.handle_mouse_event(&mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 5));
        app.handle_mouse_event(&mouse_event(MouseEventKind::Drag(MouseButton::Left), 10, 7));
        app.handle_mouse_event(&mouse_event(MouseEventKind::Up(MouseButton::Left), 10, 7));

        let sel = app.selection.expect("drag leaves a finalized selection");
        assert!(!sel.dragging);
        // Inner area starts at (1,1): down(5,5) → (line 4, col 4),
        // up(10,7) → (line 6, col 9).
        assert_eq!((sel.start_line, sel.start_col), (4, 4));
        assert_eq!((sel.end_line, sel.end_col), (6, 9));
    }

    // ── Paste placeholders ──────────────────────────────────────

    #[test]
    fn test_paste_multi_line_becomes_placeholder() {
        let mut app = make_app();
        app.handle_paste("alpha\nbeta\ngamma");
        assert_eq!(app.input, "[Pasted text #1 +3 lines]");
        assert_eq!(app.input_cursor, app.input.len());
        assert_eq!(app.paste_store.len(), 1);
    }

    #[test]
    fn test_paste_single_line_inserts_literally() {
        let mut app = make_app();
        app.handle_paste("just one line");
        assert_eq!(app.input, "just one line");
        assert_eq!(app.paste_store.len(), 0, "single-line paste needs no block");
    }

    #[test]
    fn test_paste_normalizes_crlf() {
        let mut app = make_app();
        app.handle_paste("a\r\nb");
        assert_eq!(app.input, "[Pasted text #1 +2 lines]");
        // The stored content must be '\n'-only.
        let expanded = app.paste_store.expand_all(&app.input);
        assert_eq!(expanded, "a\nb");
    }

    #[test]
    fn test_paste_dropped_while_modal_active() {
        let mut app = make_app();
        app.show_help = true;
        app.handle_paste("a\nb");
        assert!(app.input.is_empty());
        assert_eq!(app.paste_store.len(), 0);
    }

    #[test]
    fn test_backspace_deletes_placeholder_atomically() {
        let mut app = make_app();
        app.handle_paste("a\nb");
        let backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        app.handle_key(backspace);
        assert_eq!(app.input, "");
        assert_eq!(app.input_cursor, 0);
        assert_eq!(
            app.paste_store.len(),
            0,
            "block removed with its placeholder"
        );
    }

    #[test]
    fn test_backspace_after_placeholder_deletes_one_char() {
        let mut app = make_app();
        app.handle_paste("a\nb");
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        // Only the typed 'x' is gone; the placeholder survives.
        assert_eq!(app.input, "[Pasted text #1 +2 lines]");
        assert_eq!(app.paste_store.len(), 1);
    }

    #[test]
    fn test_enter_expands_placeholders_into_one_message() {
        let mut app = make_app();
        app.messages.clear();
        app.input = "review: ".to_string();
        app.input_cursor = app.input.len();
        app.handle_paste("line one\nline two");

        let result = submit_via_enter(&mut app);
        match result {
            Some(TuiCommand::RunAgent(content)) => {
                assert_eq!(content, "review: line one\nline two");
            }
            other => panic!("expected RunAgent, got {other:?}"),
        }
        assert_eq!(app.paste_store.len(), 0, "store cleared on submit");
        // History stores the expanded form so Up-recall never needs blocks.
        assert_eq!(
            app.history.last().map(|entry| entry.as_str()),
            Some("review: line one\nline two")
        );
    }

    #[test]
    fn test_enter_expands_placeholders_while_streaming() {
        let mut app = make_app();
        app.streaming = true;
        app.handle_paste("hint line 1\nhint line 2");

        let result = submit_via_enter(&mut app);
        assert!(result.is_none(), "streaming inject returns no command");
        let pending = app
            .pending_hints
            .lock()
            .expect("pending hints lock poisoned");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].content, "hint line 1\nhint line 2");
    }

    #[test]
    fn test_hand_typed_placeholder_passes_through_untouched() {
        let mut app = make_app();
        app.messages.clear();
        // No paste block is registered — the literal text is the message.
        app.input = "what does [Pasted text #1 +2 lines] mean?".to_string();
        app.input_cursor = app.input.len();

        let result = submit_via_enter(&mut app);
        match result {
            Some(TuiCommand::RunAgent(content)) => {
                assert_eq!(content, "what does [Pasted text #1 +2 lines] mean?");
            }
            other => panic!("expected RunAgent, got {other:?}"),
        }
    }

    #[test]
    fn test_unbound_ctrl_letter_does_not_insert_character() {
        let mut app = make_app();
        // Ctrl+Z is not a bound shortcut — it must not insert a literal 'z'
        // (crossterm reports Ctrl+letters as Char(c) + CONTROL).
        app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(app.input, "");

        // A plain 'z' (no CONTROL) still inserts normally.
        app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(app.input, "z");
    }

    // ── Test Helpers ────────────────────────────────────────────

    /// Simulates Enter: calls handle_key with Enter, returns the command.
    fn submit_via_enter(app: &mut App) -> Option<TuiCommand> {
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        app.handle_key(key)
    }
}
