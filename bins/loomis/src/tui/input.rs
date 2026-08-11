//! Keyboard handling, slash commands, thread picker, and shell confirmation.
//!
//! All input processing that was previously in the monolithic [`super::app`].
//! These are [`super::app::App`] methods split into their own file for readability.

use std::sync::atomic::Ordering;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use provider::{Message, Role};

use super::app::App;
use super::keyboard::{
    cancel_shortcut_label, copy_shortcut_label, has_shortcut_modifier, is_cancel_shortcut,
    is_copy_shortcut, is_paste_shortcut, paste_shortcut_label,
};
use super::messages::{
    ChatMessage, SLASH_COMMANDS, SlashCompletionState, TuiCommand, is_valid_thread_name,
    truncate_for_display,
};
use super::paste::normalize_newlines;
use crate::hooks::insert_before_history;
use sandbox::shell_filter::CommandVerdict;

// ── Paste Handling ───────────────────────────────────────────────────────────────

impl App {
    /// `true` while a modal (thread picker, intervention prompt, shell
    /// confirmation, help overlay) owns keyboard input — the same set of
    /// intercepts [`App::handle_key`] applies at its top.
    pub(super) fn is_modal_active(&self) -> bool {
        self.thread_picker.is_some()
            || self.has_pending_intervene()
            || self.pending_shell_confirm.is_some()
            || self.show_help
    }

    /// Reads the system clipboard and runs its text through the paste
    /// pipeline. No-op when the clipboard is empty or holds non-text data.
    ///
    /// Used by the Ctrl+V binding and by right-click paste in terminals
    /// that forward the right mouse button to the application.
    pub fn paste_from_clipboard(&mut self) {
        if let Ok(text) = arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
            self.handle_paste(&text);
        }
    }

    /// Handles pasted text from any source: a bracketed-paste event
    /// (Unix terminals), a coalesced key-event burst (Windows — see the
    /// event loop), or a direct clipboard read (Ctrl+V / right-click).
    ///
    /// A multi-line paste is stored in the [`super::paste::PasteStore`] and
    /// appears in the input as a compact `[Pasted text #N +M lines]`
    /// placeholder (Claude Code style); a single-line paste is inserted
    /// like typed text. The real content is restored on submit.
    pub fn handle_paste(&mut self, text: &str) {
        // A modal owns the input focus — pasting into it would insert text
        // its key handler doesn't expect, so the event is dropped.
        if self.is_modal_active() {
            tracing::trace!("Paste event dropped while a modal is active");
            return;
        }

        let normalized = normalize_newlines(text);
        tracing::info!(
            paste_len = text.len(),
            normalized_len = normalized.len(),
            has_newline = normalized.contains('\n'),
            has_cr = text.contains('\r'),
            newline_count = normalized.chars().filter(|&c| c == '\n').count(),
            "handle_paste called"
        );
        if normalized.is_empty() {
            return;
        }

        let inserted = if normalized.contains('\n') {
            self.paste_store.add_text(normalized)
        } else {
            normalized
        };
        self.input.insert_str(self.input_cursor, &inserted);
        self.input_cursor += inserted.len();

        // A pasted `/`-prefix deserves the same completion popup as a
        // typed one.
        if !self.streaming && !self.slash_dismissed && self.input.starts_with('/') {
            self.update_slash_completion();
        }
    }
}

// ── Keyboard Handling ────────────────────────────────────────────────────────────

impl App {
    /// Processes a single key event. Returns `Some(TuiCommand)` when the
    /// key sequence triggers an action that needs the agent thread.
    ///
    /// Slash commands (`/stats`, `/tools`) are handled inline because they
    /// only need shared state already available on the TUI side.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<TuiCommand> {
        // ── Thread picker intercepts most keys ───────────────────
        if self.thread_picker.is_some() {
            return self.handle_thread_picker_key(key);
        }

        // ── Intervention prompt intercepts most keys ────────────
        if self.has_pending_intervene() {
            return self.handle_intervene_key(key);
        }

        // ── Shell confirmation (y/n) intercepts most keys ───────
        if self.pending_shell_confirm.is_some() {
            return self.handle_shell_confirm_key(key);
        }

        // ── Help overlay swallows keys until dismissed ──────────
        if self.show_help {
            match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') | KeyCode::F(1) => {
                    self.show_help = false;
                }
                _ => {} // swallow everything else while help is open
            }
            return None;
        }

        // ── F1 opens help from anywhere (except modals above) ───
        if key.code == KeyCode::F(1) {
            self.show_help = true;
            return None;
        }

        // ── Slash completion intercepts navigation/editing ──────
        if self.slash_completion.is_some() {
            return self.handle_slash_completion_key(key);
        }

        match key.code {
            // ── Clipboard shortcuts — platform-aware ──────────
            // Wildcard arms: the shortcut helpers accept any key code, so
            // they must run before the generic `Char(c)` insertion arm
            // below. The bindings are OS-native — Cmd+C/V on macOS,
            // Ctrl+C/V elsewhere (see [`super::keyboard`]).
            _ if is_paste_shortcut(&key) => {
                // Terminals that handle the paste chord themselves never
                // send this key (their paste arrives via bracketed paste /
                // a key-event burst instead). Terminals that DON'T forward
                // it — read the clipboard directly so paste works there too.
                self.paste_from_clipboard();
                None
            }
            _ if is_copy_shortcut(&key) || is_cancel_shortcut(&key) => {
                self.handle_copy_or_cancel(&key)
            }

            // ── Submit / Newline ───────────────────────────────
            KeyCode::Enter => {
                // Shift+Enter inserts a newline
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.input.insert(self.input_cursor, '\n');
                    self.input_cursor += 1;
                    return None;
                }

                // Any real submission ends the completion session.
                self.slash_completion = None;
                self.slash_dismissed = false;

                let raw_input = self.input.trim().to_string();
                if raw_input.is_empty() {
                    self.input.clear();
                    self.input_cursor = 0;
                    return None;
                }

                // Restore pasted content: `[Pasted text #N …]` placeholders
                // stand in for text kept in the paste store. The bang/slash
                // checks below deliberately inspect the RAW input so a
                // paste can never smuggle a command past the user; only the
                // message sent to the agent is expanded.
                let expanded_input = self.paste_store.expand_all(&raw_input);
                if expanded_input.trim().is_empty() {
                    // The placeholder was edited beyond recognition.
                    self.input.clear();
                    self.input_cursor = 0;
                    self.paste_store.clear();
                    return None;
                }

                // Save to history — the expanded form, so recalling it with
                // Up never depends on paste blocks that no longer exist.
                self.history.push(expanded_input.clone());
                self.history_index = None;
                self.draft_input.clear();

                // ── Inject mode: agent is running ──────────────────
                if self.streaming {
                    {
                        // Queue hint in pending_hints instead of pushing
                        // directly to memory — avoids inserting a user
                        // message between an assistant tool_calls message
                        // and its tool results (API contract violation).
                        let mut pending = self
                            .pending_hints
                            .lock()
                            .expect("pending hints lock poisoned");
                        pending.push(Message::new(Role::User, expanded_input.clone()));
                    }
                    self.messages.push(ChatMessage::User {
                        content: expanded_input,
                        timestamp: ChatMessage::now_timestamp(),
                    });
                    self.input.clear();
                    self.input_cursor = 0;
                    self.paste_store.clear();
                    self.auto_scroll = true;
                    self.scroll_offset = 0;
                    return None;
                }

                // Check for bang commands (!command — execute asynchronously)
                if raw_input.starts_with('!') && !raw_input.starts_with("!!") {
                    let command = raw_input[1..].trim().to_string();
                    self.input.clear();
                    self.input_cursor = 0;
                    self.paste_store.clear();
                    self.auto_scroll = true;
                    if command.is_empty() {
                        self.messages.push(ChatMessage::System {
                            content: "Usage: !<command> — runs a shell command and shares output with the agent."
                                .into(),
                            timestamp: ChatMessage::now_timestamp(),
                        });
                        return None;
                    }
                    // Classify with the same sandbox policy the SandboxHook
                    // applies to LLM-initiated shell calls (Nielsen #5).
                    match self.shell_filter.classify(&command) {
                        CommandVerdict::AutoApproved => {
                            return Some(TuiCommand::RunShell(command));
                        }
                        CommandVerdict::Blocked { reason } => {
                            self.messages.push(ChatMessage::Error {
                                content: format!("Blocked by sandbox policy: {reason}"),
                                timestamp: ChatMessage::now_timestamp(),
                            });
                            return None;
                        }
                        CommandVerdict::RequiresApproval => {
                            self.messages.push(ChatMessage::System {
                                content: format!("Run shell command `!{command}`? (y/n)"),
                                timestamp: ChatMessage::now_timestamp(),
                            });
                            self.pending_shell_confirm = Some(command);
                            return None;
                        }
                    }
                }

                // Check for slash commands
                if let Some(cmd) = self.handle_slash_command(&raw_input) {
                    self.input.clear();
                    self.input_cursor = 0;
                    self.paste_store.clear();
                    self.auto_scroll = true;
                    return cmd;
                }

                // Conversation titling is owned by PersistenceHook —
                // it generates an LLM title from this first query on
                // on_run_start.

                self.messages.push(ChatMessage::User {
                    content: expanded_input.clone(),
                    timestamp: ChatMessage::now_timestamp(),
                });
                self.input.clear();
                self.input_cursor = 0;
                self.paste_store.clear();
                self.auto_scroll = true;
                self.scroll_offset = 0;
                self.streaming = true;
                // Remember for Ctrl+R retry after a failure.
                self.last_submitted_input = Some(expanded_input.clone());

                Some(TuiCommand::RunAgent(expanded_input))
            }

            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.input.is_empty() && !self.streaming {
                    self.should_quit = true;
                    return Some(TuiCommand::Exit);
                }
                // Otherwise: delete forward
                self.delete_at_cursor();
                None
            }

            // ── Retry last submission (Nielsen #9) ───────────────
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.streaming {
                    return None;
                }
                let last = self.last_submitted_input.clone()?;
                // Re-submit the previous input as-is. Not pushed to history
                // again — it's already there from the first submission.
                self.messages.push(ChatMessage::User {
                    content: last.clone(),
                    timestamp: ChatMessage::now_timestamp(),
                });
                self.auto_scroll = true;
                self.scroll_offset = 0;
                self.streaming = true;
                Some(TuiCommand::RunAgent(last))
            }

            KeyCode::Esc => {
                // If there's a selection, clear it first.
                if self.selection.is_some() {
                    self.selection = None;
                    return None;
                }
                if self.streaming {
                    self.streaming = false;
                    return Some(TuiCommand::CancelGeneration);
                }
                None
            }

            // ── Scrolling ──────────────────────────────────────
            KeyCode::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(8);
                self.auto_scroll = false;
                None
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(8);
                if self.scroll_offset == 0 {
                    self.auto_scroll = true;
                }
                None
            }

            // ── Multi-line / History navigation ────────────────
            //
            // Multi-line cursor Up algorithm:
            // 1. Find the start of the current line (rfind '\n' or 0).
            // 2. Find the start of the previous line.
            // 3. Compute the cursor's column offset within the current line.
            // 4. Clamp that offset to the length of the previous line.
            // 5. Set cursor to prev_line_start + clamped_offset.
            // If the cursor is already on the first line, fall through
            // to input history navigation.
            KeyCode::Up => {
                // If not navigating history and cursor is below first line,
                // move cursor up within multi-line input.
                if self.history_index.is_none() {
                    let cursor_line = self.input[..self.input_cursor]
                        .chars()
                        .filter(|&c| c == '\n')
                        .count();
                    if cursor_line > 0 {
                        // Find the start of the current line
                        let line_start = self.input[..self.input_cursor]
                            .rfind('\n')
                            .map(|p| p + 1)
                            .unwrap_or(0);
                        // Find the start of the previous line
                        if let Some(prev_start) =
                            self.input[..line_start.saturating_sub(1)].rfind('\n')
                        {
                            let prev_start = prev_start + 1;
                            let prev_line_len = line_start.saturating_sub(prev_start + 1);
                            // Position cursor at same column, clamped to line length
                            let col_in_line = self.input_cursor.saturating_sub(line_start);
                            let new_col = col_in_line.min(prev_line_len);
                            self.input_cursor = prev_start + new_col;
                        } else {
                            // First line — column clamped
                            let col_in_line = self.input_cursor.saturating_sub(line_start);
                            let new_col = col_in_line.min(line_start.saturating_sub(1));
                            self.input_cursor = new_col;
                        }
                        return None;
                    }
                }

                // Fall through to history navigation
                if self.history.is_empty() {
                    return None;
                }
                if self.history_index.is_none() {
                    self.draft_input = self.input.clone();
                    self.history_index = Some(self.history.len());
                }
                if let Some(ref mut idx) = self.history_index
                    && *idx > 0
                {
                    *idx -= 1;
                    self.input = self.history[*idx].clone();
                    self.input_cursor = self.input.len();
                }
                None
            }
            // Multi-line cursor Down: mirror of the Up algorithm.
            // 1. Find the end of the current line.
            // 2. Find the end of the next line.
            // 3. Compute column offset within the current line.
            // 4. Clamp to the next line's length.
            // 5. Set cursor to next_line_start + clamped_offset.
            // Falls through to history navigation when already on the last line.
            KeyCode::Down => {
                // If not navigating history, try to move cursor down in multi-line input.
                if self.history_index.is_none() {
                    let total_lines = self.input.chars().filter(|&c| c == '\n').count() + 1;
                    let cursor_line = self.input[..self.input_cursor]
                        .chars()
                        .filter(|&c| c == '\n')
                        .count();
                    if cursor_line + 1 < total_lines {
                        // Find end of current line
                        let line_end = self.input[self.input_cursor..]
                            .find('\n')
                            .map(|p| self.input_cursor + p)
                            .unwrap_or(self.input.len());
                        // Start of next line
                        let next_line_start = line_end + 1;
                        // End of next line
                        let next_line_end = self.input[next_line_start..]
                            .find('\n')
                            .map(|p| next_line_start + p)
                            .unwrap_or(self.input.len());
                        // Position at same column, clamped to next line length
                        let line_start = self.input[..self.input_cursor]
                            .rfind('\n')
                            .map(|p| p + 1)
                            .unwrap_or(0);
                        let col_in_line = self.input_cursor.saturating_sub(line_start);
                        let next_line_len = next_line_end.saturating_sub(next_line_start);
                        let new_col = col_in_line.min(next_line_len);
                        self.input_cursor = next_line_start + new_col;
                        return None;
                    }
                }

                // Fall through to history navigation
                if let Some(ref mut idx) = self.history_index {
                    if *idx + 1 < self.history.len() {
                        *idx += 1;
                        self.input = self.history[*idx].clone();
                    } else {
                        // End of history — restore draft
                        self.history_index = None;
                        self.input = self.draft_input.clone();
                    }
                    self.input_cursor = self.input.len();
                }
                None
            }

            // ── Cursor movement ────────────────────────────────
            KeyCode::Home => {
                self.input_cursor = 0;
                None
            }
            KeyCode::End => {
                self.input_cursor = self.input.len();
                None
            }
            KeyCode::Left => {
                if self.input_cursor > 0 {
                    self.input_cursor = self.prev_char_boundary();
                }
                None
            }
            KeyCode::Right => {
                if self.input_cursor < self.input.len() {
                    self.input_cursor = self.next_char_boundary();
                }
                None
            }

            // ── Editing ────────────────────────────────────────
            KeyCode::Backspace => {
                if self.input_cursor > 0 {
                    // A paste placeholder is deleted atomically: it stands
                    // for content held in the paste store, so the text and
                    // the stored block are dropped together.
                    let placeholder = self
                        .paste_store
                        .placeholder_suffix(&self.input[..self.input_cursor]);
                    if let Some(placeholder) = placeholder {
                        let placeholder_start = self.input_cursor - placeholder.len();
                        self.input.drain(placeholder_start..self.input_cursor);
                        self.input_cursor = placeholder_start;
                        self.paste_store.remove_by_placeholder(&placeholder);
                    } else {
                        let prev = self.prev_char_boundary();
                        self.input.remove(prev);
                        self.input_cursor = prev;
                    }
                }
                // Editing the filter text starts a new completion session.
                self.slash_dismissed = false;
                None
            }
            KeyCode::Delete => {
                if self.input_cursor < self.input.len() {
                    self.delete_at_cursor();
                }
                None
            }

            // ── Character insertion ────────────────────────────
            KeyCode::Char(c) => {
                // If there's a pending intervention prompt, route to
                // the intervention key handler.
                if self.has_pending_intervene() {
                    return self.handle_intervene_key(key);
                }

                // Unbound modifier+letter chords are shortcuts we don't
                // handle, not text — crossterm reports them as Char(c) +
                // CONTROL (plus SUPER on macOS), and without this guard
                // they'd insert a literal letter (Ctrl+Z inserting 'z',
                // Cmd+Z inserting 'z' on macOS).
                if has_shortcut_modifier(&key) {
                    return None;
                }

                // `?` on an empty, idle input opens the help overlay
                // instead of inserting a character (Nielsen #10).
                if c == '?' && self.input.is_empty() && !self.streaming {
                    self.show_help = true;
                    return None;
                }

                // On some terminals Shift+Enter sends a newline char
                // (handled above via Enter). Plain char insertion:
                self.input.insert(self.input_cursor, c);
                self.input_cursor += c.len_utf8();

                // Activate slash completion when typing a `/` prefix.
                if !self.streaming && !self.slash_dismissed && self.input.starts_with('/') {
                    self.update_slash_completion();
                }
                None
            }

            _ => None,
        }
    }
}

impl App {
    /// Handles the copy / cancel chord — the platform-sensitive part of
    /// `Ctrl+C`-style handling (see [`super::keyboard`]).
    ///
    /// Priority order (first match wins):
    /// 1. **Copy** — a finalized text selection + the platform copy
    ///    shortcut (`Cmd+C` on macOS, `Ctrl+C` elsewhere).
    /// 2. **Cancel** — the agent is streaming + the cancel shortcut
    ///    (`Ctrl+C` on every platform).
    /// 3. **Hint** — idle; point at the real exit bindings instead of
    ///    quitting (Nielsen #4: consistency).
    fn handle_copy_or_cancel(&mut self, key: &KeyEvent) -> Option<TuiCommand> {
        // Copy the selection to the system clipboard and clear the
        // highlight. On macOS this fires for Cmd+C only — Ctrl+C never
        // copies there, it stays the pure interrupt key. (Terminals that
        // intercept Cmd+C never send it here at all; on macOS the
        // selection is also copied on mouse-up — see [`App::handle_mouse_event`].)
        if is_copy_shortcut(key)
            && let Some(ref sel) = self.selection
            && !sel.dragging
        {
            self.copy_selection_to_clipboard();
            self.selection = None;
            return None;
        }

        // Cancel streaming — the "interrupt" chord.
        if is_cancel_shortcut(key) {
            if self.streaming {
                self.streaming = false;
                return Some(TuiCommand::CancelGeneration);
            }
            // Idle: Ctrl+C does NOT quit — the key means copy/cancel
            // everywhere (Nielsen #4). Point at the real exit bindings;
            // on macOS also name the copy key since it differs from Ctrl+C.
            let content = if cfg!(target_os = "macos") {
                format!(
                    "Press Ctrl+D to exit, or type /exit. ({} copies selected text)",
                    copy_shortcut_label()
                )
            } else {
                "Press Ctrl+D to exit, or type /exit.".into()
            };
            self.messages.push(ChatMessage::System {
                content,
                timestamp: ChatMessage::now_timestamp(),
            });
        }
        None
    }
}

// ── Slash Commands ───────────────────────────────────────────────────────────────

impl App {
    /// Key handling while a `!command` is awaiting y/n confirmation.
    ///
    /// `y`/`Enter` executes, `n`/`Esc` cancels; everything else is
    /// swallowed so the confirmation can't be bypassed accidentally.
    fn handle_shell_confirm_key(&mut self, key: KeyEvent) -> Option<TuiCommand> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                let cmd = self
                    .pending_shell_confirm
                    .take()
                    .expect("pending_shell_confirm checked before dispatch");
                self.auto_scroll = true;
                Some(TuiCommand::RunShell(cmd))
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                let cmd = self
                    .pending_shell_confirm
                    .take()
                    .expect("pending_shell_confirm checked before dispatch");
                self.messages.push(ChatMessage::System {
                    content: format!("Shell command cancelled: !{cmd}"),
                    timestamp: ChatMessage::now_timestamp(),
                });
                None
            }
            _ => None,
        }
    }

    /// Recomputes the completion popup contents from the current input.
    ///
    /// Called after every character insertion while the input starts with
    /// `/`. Shows all commands on a bare `/`, filters by name prefix as the
    /// user types, and closes the popup when nothing matches.
    fn update_slash_completion(&mut self) {
        let filter = self.input[1..].to_lowercase();
        let matches: Vec<&'static super::messages::CommandInfo> = SLASH_COMMANDS
            .iter()
            .filter(|c| filter.is_empty() || c.name.starts_with(&filter))
            .collect();

        if matches.is_empty() {
            self.slash_completion = None;
            return;
        }

        // Keep the previous selection when it's still in range.
        let selected = self
            .slash_completion
            .as_ref()
            .map(|s| s.selected.min(matches.len() - 1))
            .unwrap_or(0);
        self.slash_completion = Some(SlashCompletionState { matches, selected });
    }

    /// Replaces the input with the full command name of the currently
    /// highlighted completion entry.
    fn accept_slash_completion(&mut self) {
        if let Some(ref sc) = self.slash_completion
            && let Some(cmd) = sc.matches.get(sc.selected)
        {
            self.input = format!("/{} ", cmd.name);
            self.input_cursor = self.input.len();
        }
        self.slash_completion = None;
    }

    /// Key handling while the completion popup is open.
    ///
    /// Navigation keys are consumed by the popup; printable characters and
    /// Backspace edit the filter and re-filter; Esc dismisses without
    /// touching the input; Tab/Right accepts; Enter accepts and submits.
    fn handle_slash_completion_key(&mut self, key: KeyEvent) -> Option<TuiCommand> {
        match key.code {
            KeyCode::Esc => {
                // Dismiss the popup only — the typed text stays. Further
                // typing won't reopen it until the filter text changes.
                self.slash_completion = None;
                self.slash_dismissed = true;
                None
            }
            KeyCode::Up => {
                if let Some(ref mut sc) = self.slash_completion {
                    sc.selected = sc.selected.saturating_sub(1);
                }
                None
            }
            KeyCode::Down => {
                if let Some(ref mut sc) = self.slash_completion {
                    sc.selected = (sc.selected + 1).min(sc.matches.len().saturating_sub(1));
                }
                None
            }
            KeyCode::Tab | KeyCode::Right => {
                // Accept the highlighted command, keep editing.
                self.accept_slash_completion();
                None
            }
            KeyCode::Enter => {
                // Commands with required arguments (`/save <name>`) are
                // accepted but not submitted — the user still has to type
                // the argument (Nielsen #5: error prevention).
                let needs_args = self
                    .slash_completion
                    .as_ref()
                    .and_then(|sc| sc.matches.get(sc.selected))
                    .map(|cmd| cmd.usage.contains('<'))
                    .unwrap_or(false);
                self.accept_slash_completion();
                if needs_args {
                    None
                } else {
                    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
                    self.handle_key(enter)
                }
            }
            KeyCode::Backspace => {
                if self.input_cursor > 0 {
                    let prev = self.prev_char_boundary();
                    self.input.remove(prev);
                    self.input_cursor = prev;
                }
                // Deleted past the `/`? Close for good this session.
                if !self.input.starts_with('/') {
                    self.slash_completion = None;
                } else {
                    self.update_slash_completion();
                }
                None
            }
            // A modified chord (Ctrl+letter, or Cmd+letter on macOS) is a
            // shortcut, not text — re-dispatch so the main handler can
            // interpret it instead of inserting the letter.
            KeyCode::Char(c) if !has_shortcut_modifier(&key) => {
                self.input.insert(self.input_cursor, c);
                self.input_cursor += c.len_utf8();
                self.update_slash_completion();
                None
            }
            // Anything else (Home/End/Left/PgUp/…): close the popup and
            // re-dispatch through the normal handler.
            _ => {
                self.slash_completion = None;
                self.handle_key(key)
            }
        }
    }

    /// Handles slash commands that don't need the agent. Returns
    /// `Some(TuiCommand)` when the command needs agent-thread action.
    fn handle_slash_command(&mut self, input: &str) -> Option<Option<TuiCommand>> {
        // Split the command name from any trailing text so "/plan <text>"
        // and "/init <text>" are recognized as commands instead of silently
        // becoming chat messages (Nielsen #5: error prevention).
        let (cmd, rest) = match input.split_once(char::is_whitespace) {
            Some((c, r)) => (c, r.trim()),
            None => (input, ""),
        };

        // ── Argument-taking commands ──
        if cmd == "/save" {
            let name = rest;
            if name.is_empty() || !is_valid_thread_name(name) {
                self.messages.push(ChatMessage::System {
                    content: "Usage: /save <name>  —  name must not contain control characters or any of: / \\ : * ? \" < > |".into(),
                    timestamp: ChatMessage::now_timestamp(),
                });
                return Some(None);
            }
            let mem = self.memory.read().expect("memory lock poisoned");
            match persistence::save_conversation(
                name,
                &self.workspace_root,
                &mem,
                &self.persistence_config,
            ) {
                Ok(()) => {
                    let _ = persistence::write_current_thread_name(
                        name,
                        &self.workspace_root,
                        &self.persistence_config,
                    );
                    self.messages.push(ChatMessage::System {
                        content: format!("Saved conversation as \"{name}\"."),
                        timestamp: ChatMessage::now_timestamp(),
                    });
                }
                Err(e) => {
                    self.messages.push(ChatMessage::Error {
                        content: format!("Failed to save: {e}"),
                        timestamp: ChatMessage::now_timestamp(),
                    });
                }
            }
            return Some(None);
        }

        if cmd == "/resume" {
            if rest.is_empty() {
                self.open_thread_picker();
                return Some(None);
            }
            return Some(self.do_resume(rest));
        }

        // ── /skill <name> — load a named skill ──
        if cmd == "/skill" {
            let name = rest;
            if name.is_empty() {
                let available = self.skill_registry.names().join(", ");
                let content = if available.is_empty() {
                    "No skills available. Define skill .md files in .loomis/skills/.".into()
                } else {
                    format!("Usage: /skill <name>\nAvailable: {available}")
                };
                self.messages.push(ChatMessage::System {
                    content,
                    timestamp: ChatMessage::now_timestamp(),
                });
                return Some(None);
            }

            match self.skill_registry.by_name(name) {
                Some(skill) => {
                    // Add to active skills for the hook to maintain.
                    if let Ok(mut active) = self.active_skills.write() {
                        active.insert(skill.name.clone(), skill.content.clone());
                    }
                    // Inject directly into memory for immediate effect.
                    // Use insert_before_history so the message lands at the
                    // tail of the System block (SkillHook will clean it up
                    // and re-insert on the next on_llm_start).
                    let msg = format!("[SKILL: {}]\n\n{}", skill.name, skill.content);
                    {
                        let mut mem = self.memory.write().expect("memory lock poisoned");
                        insert_before_history(&mut mem.messages, Message::new(Role::System, msg));
                    }
                    self.messages.push(ChatMessage::System {
                        content: format!("Loaded skill \"{}\" — {}", skill.name, skill.description),
                        timestamp: ChatMessage::now_timestamp(),
                    });
                }
                None => {
                    let available = self.skill_registry.names().join(", ");
                    self.messages.push(ChatMessage::Error {
                        content: format!("Unknown skill \"{name}\". Available: [{available}]"),
                        timestamp: ChatMessage::now_timestamp(),
                    });
                }
            }
            return Some(None);
        }

        // ── Commands that take no arguments (or ignore them) ──
        match cmd {
            "/exit" => {
                self.should_quit = true;
                Some(Some(TuiCommand::Exit))
            }

            "/new" => {
                // Save current conversation before starting fresh.
                if let Some(ref title) = self.conversation_title {
                    let mem = self.memory.read().expect("memory lock poisoned");
                    let _ = persistence::save_conversation(
                        title,
                        &self.workspace_root,
                        &mem,
                        &self.persistence_config,
                    );
                }
                self.conversation_title = None;
                // Write fallback for the gap between /new and first message.
                let _ = persistence::write_current_thread_name(
                    &self.persistence_config.default_thread_name,
                    &self.workspace_root,
                    &self.persistence_config,
                );

                self.messages.clear();
                self.messages.push(ChatMessage::System {
                    content: "New conversation started (system prompt preserved).".into(),
                    timestamp: ChatMessage::now_timestamp(),
                });
                Some(Some(TuiCommand::ClearConversation))
            }

            "/plan" => {
                // "/plan <text>" means "enter plan mode and make this plan" —
                // the text is forwarded to the agent; a bare "/plan" keeps
                // its documented toggle behaviour.
                let was_active = self.plan_mode.active.load(Ordering::SeqCst);
                let new_state = if rest.is_empty() { !was_active } else { true };
                self.plan_mode.active.store(new_state, Ordering::SeqCst);

                let plan_path = self.workspace_root.join(".loomis").join("plan.md");
                let content = if new_state && !was_active {
                    // Ensure the .loomis directory exists.
                    if let Some(parent) = plan_path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    format!(
                        "Plan mode activated. Plan file: {}\nUse /plan again to deactivate, or /approve to exit plan mode.",
                        plan_path.display()
                    )
                } else if !new_state {
                    "Plan mode deactivated. Full access restored.".into()
                } else {
                    // "/plan <text>" while already in plan mode — the request
                    // is forwarded below; no state-change announcement.
                    String::new()
                };

                if !content.is_empty() {
                    self.messages.push(ChatMessage::System {
                        content,
                        timestamp: ChatMessage::now_timestamp(),
                    });
                }

                if rest.is_empty() {
                    Some(None)
                } else {
                    // Conversation titling is owned by PersistenceHook —
                    // it generates an LLM title from this first query on
                    // on_run_start.
                    // Show the user's message in the chat — without this it
                    // reaches the agent but never appears on screen.
                    self.messages.push(ChatMessage::User {
                        content: rest.to_string(),
                        timestamp: ChatMessage::now_timestamp(),
                    });
                    self.scroll_offset = 0;
                    self.streaming = true;
                    self.last_submitted_input = Some(rest.to_string());
                    Some(Some(TuiCommand::RunAgent(rest.to_string())))
                }
            }

            "/approve" => {
                if self.plan_mode.active.load(Ordering::SeqCst) {
                    // Archive the plan before deactivating.
                    let plan_path = self.workspace_root.join(".loomis").join("plan.md");
                    let plan_dir = self.plan_dir.clone();
                    let archive_msg = match std::fs::read_to_string(&plan_path) {
                        Ok(content) if !content.trim().is_empty() => {
                            // Use the archive_plan helper.
                            match crate::tools::archive_plan(&content, &plan_dir) {
                                Ok(archived_path) => {
                                    format!("Plan archived to: {}", archived_path.display())
                                }
                                Err(e) => format!("Warning: failed to archive plan: {e}"),
                            }
                        }
                        _ => String::new(),
                    };

                    self.plan_mode.active.store(false, Ordering::SeqCst);
                    let content = if archive_msg.is_empty() {
                        "Plan approved! Plan mode deactivated. You can now execute the plan.".into()
                    } else {
                        format!(
                            "Plan approved! Plan mode deactivated. {archive_msg}. \
                             You can now execute the plan."
                        )
                    };
                    self.messages.push(ChatMessage::System {
                        content,
                        timestamp: ChatMessage::now_timestamp(),
                    });
                } else {
                    self.messages.push(ChatMessage::System {
                        content: "Not in plan mode. Use /plan first to enter plan mode.".into(),
                        timestamp: ChatMessage::now_timestamp(),
                    });
                }
                Some(None)
            }

            "/threads" => {
                self.open_thread_picker();
                Some(None)
            }

            "/stats" => {
                let mem = self.memory.read().expect("memory lock poisoned");
                let content = format!(
                    "Messages: {}  |  Characters: {}",
                    mem.len(),
                    mem.total_chars(),
                );
                self.messages.push(ChatMessage::System {
                    content,
                    timestamp: ChatMessage::now_timestamp(),
                });
                Some(None)
            }

            "/tools" => {
                let content = if self.tool_names.is_empty() {
                    "No tools registered.".to_string()
                } else {
                    self.tool_names
                        .iter()
                        .enumerate()
                        .map(|(i, name)| format!("  {}. {}", i + 1, name))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                self.messages.push(ChatMessage::System {
                    content,
                    timestamp: ChatMessage::now_timestamp(),
                });
                Some(None)
            }

            "/init" => {
                let init_prompt = include_str!("../../prompts/init.md");
                // Trailing text is appended as an additional instruction —
                // "/init 记得带上日志" tells the agent what to focus on.
                let prompt = if rest.is_empty() {
                    init_prompt.to_string()
                } else {
                    format!("{init_prompt}\n\n### Additional instruction from the user\n\n{rest}")
                };
                self.messages.push(ChatMessage::System {
                    content: "Initializing project documentation…\n\
                              I'll explore the codebase, ask a few questions, \
                              and create or update LOOMIS.md."
                        .into(),
                    timestamp: ChatMessage::now_timestamp(),
                });
                Some(Some(TuiCommand::RunAgent(prompt)))
            }

            "/help" => {
                // Clipboard shortcuts are OS-native — Cmd on macOS, Ctrl
                // elsewhere (see [`super::keyboard`]). On macOS the copy
                // and cancel keys differ; on other platforms one key does
                // both, which the two lines below make explicit.
                let copy_shortcut = format!("  {:<13} — copy selected text", copy_shortcut_label());
                let paste_shortcut =
                    format!("  {:<13} — paste from clipboard", paste_shortcut_label());
                let cancel_shortcut = format!(
                    "  {:<13} — cancel generation / exit",
                    cancel_shortcut_label()
                );

                let content = [
                    "Commands:",
                    "  /exit          — quit",
                    "  /new           — start a new conversation",
                    "  /init [text]   — initialize or update project rules (LOOMIS.md);",
                    "                    text = extra instruction for the init agent",
                    "  /plan [text]   — toggle plan mode (read-only research & planning);",
                    "                    with text, enter plan mode and make the plan",
                    "  /approve       — approve plan and exit plan mode",
                    "  /save <name>   — save conversation as a named thread",
                    "  /resume [name] — restore a thread (no name = picker)",
                    "  /threads       — open thread picker",
                    "  /stats         — memory statistics",
                    "  /tools         — list registered tools",
                    "  /skill <name>  — load a named skill",
                    "  /help          — show this message",
                    "",
                    "Shell prefix:",
                    "  !<cmd>  — run a shell command and share output with the agent",
                    "  !!text  — literal text starting with '!' (not a shell command)",
                    "  Example: !dir, !git status, !cargo test",
                    "",
                    "Keys:",
                    "  Enter        — send message",
                    "  Shift+Enter  — newline",
                    "  PgUp/PgDown/🖱 — scroll chat",
                    "  Up/Down      — input history / multi-line nav",
                    copy_shortcut.as_str(),
                    paste_shortcut.as_str(),
                    cancel_shortcut.as_str(),
                    "  Esc          — cancel generation",
                    "  Y / n        — approve / deny shell command",
                ]
                .join("\n");
                self.messages.push(ChatMessage::System {
                    content,
                    timestamp: ChatMessage::now_timestamp(),
                });
                Some(None)
            }

            _ => None, // not a slash command — normal message
        }
    }

    // ── Thread Picker ─────────────────────────────────────────────────────────

    /// Handles keyboard input while the thread picker overlay is active.
    ///
    /// Only `Esc`, `Enter`, `Up`, and `Down` are processed; all other keys
    /// are swallowed to prevent input from leaking into the chat.
    fn handle_thread_picker_key(&mut self, key: KeyEvent) -> Option<TuiCommand> {
        let Some(picker) = &mut self.thread_picker else {
            return None;
        };

        match key.code {
            KeyCode::Esc => {
                self.thread_picker = None;
                None
            }
            KeyCode::Enter => {
                let name = picker.threads[picker.selected].name.clone();
                self.thread_picker = None;
                self.do_resume(&name)
            }
            KeyCode::Up => {
                if picker.selected > 0 {
                    picker.selected -= 1;
                }
                None
            }
            KeyCode::Down => {
                if picker.selected + 1 < picker.threads.len() {
                    picker.selected += 1;
                }
                None
            }
            _ => None, // swallow all other keys
        }
    }

    /// Loads a named thread and replaces the current conversation.
    ///
    /// Shared by the picker (`Enter`) and the `/resume <name>` slash command.
    fn do_resume(&mut self, name: &str) -> Option<TuiCommand> {
        match persistence::load_conversation(name, &self.workspace_root, &self.persistence_config) {
            Ok(loaded) => {
                *self.memory.write().expect("memory lock poisoned") = loaded;
                let _ = persistence::write_current_thread_name(
                    name,
                    &self.workspace_root,
                    &self.persistence_config,
                );
                self.conversation_title = Some(name.to_string());
                self.rebuild_messages_from_memory();
                self.messages.insert(
                    0,
                    ChatMessage::System {
                        content: format!("Resumed conversation \"{name}\"."),
                        timestamp: ChatMessage::now_timestamp(),
                    },
                );
            }
            Err(e) => {
                self.messages.push(ChatMessage::Error {
                    content: format!("Failed to resume \"{name}\": {e}"),
                    timestamp: ChatMessage::now_timestamp(),
                });
            }
        }
        None
    }

    /// Opens the thread picker overlay with all saved conversations.
    fn open_thread_picker(&mut self) {
        match persistence::list_threads(&self.workspace_root, &self.persistence_config) {
            Ok(threads) if !threads.is_empty() => {
                self.thread_picker = Some(super::messages::ThreadPicker {
                    threads,
                    selected: 0,
                });
            }
            Ok(_) => {
                self.messages.push(ChatMessage::System {
                    content: "No saved conversations. Use /save <name> to save one.".into(),
                    timestamp: ChatMessage::now_timestamp(),
                });
            }
            Err(e) => {
                self.messages.push(ChatMessage::Error {
                    content: format!("Error listing threads: {e}"),
                    timestamp: ChatMessage::now_timestamp(),
                });
            }
        }
    }

    /// Rebuilds `self.messages` (TUI display) from the current state of
    /// `self.memory`. Used after `/resume` to restore display history.
    fn rebuild_messages_from_memory(&mut self) {
        let mem = self.memory.read().expect("memory lock poisoned");
        let msgs = mem.messages().to_vec(); // clone under lock, then drop
        drop(mem);

        let ts = ChatMessage::now_timestamp();
        self.messages.clear();

        for msg in &msgs {
            match msg.role {
                Role::System => {
                    // System messages in memory are LLM context (system prompt,
                    // environment info, project rules) — skip them so they
                    // don't clutter the chat display after /resume.
                }
                Role::User => {
                    self.messages.push(ChatMessage::User {
                        content: msg.content.clone(),
                        timestamp: ts.clone(),
                    });
                }
                Role::Assistant => {
                    // Append compact tool-call summary if present
                    let content = if let Some(ref tool_calls) = msg.tool_calls {
                        let tc_list: Vec<String> = tool_calls
                            .iter()
                            .map(|tc| format!("[Tool: {} (id: {})]", tc.function.name, tc.id))
                            .collect();
                        if msg.content.is_empty() {
                            tc_list.join("\n")
                        } else {
                            format!("{}\n\n{}", msg.content, tc_list.join("\n"))
                        }
                    } else {
                        msg.content.clone()
                    };
                    self.messages.push(ChatMessage::Assistant {
                        content,
                        timestamp: ts.clone(),
                    });
                }
                Role::Tool => {
                    let preview = truncate_for_display(&msg.content, 500);
                    let id = msg.tool_call_id.as_deref().unwrap_or("?");
                    self.messages.push(ChatMessage::System {
                        content: format!("[Tool result: {id}]\n{preview}"),
                        timestamp: ts.clone(),
                    });
                }
                _ => {}
            }
        }

        self.auto_scroll = true;
        self.scroll_offset = 0;
    }
}

// ── Unicode-safe editing helpers ─────────────────────────────────────────────────

impl App {
    /// Returns the byte position of the previous UTF-8 char boundary.
    fn prev_char_boundary(&self) -> usize {
        if self.input_cursor == 0 {
            return 0;
        }
        let mut pos = self.input_cursor - 1;
        while !self.input.is_char_boundary(pos) {
            pos -= 1;
        }
        pos
    }

    /// Returns the byte position of the next UTF-8 char boundary.
    fn next_char_boundary(&self) -> usize {
        if self.input_cursor >= self.input.len() {
            return self.input.len();
        }
        let mut pos = self.input_cursor + 1;
        while pos < self.input.len() && !self.input.is_char_boundary(pos) {
            pos += 1;
        }
        pos
    }

    /// Deletes the character at (after) the cursor position.
    fn delete_at_cursor(&mut self) {
        if self.input_cursor < self.input.len() {
            let next = self.next_char_boundary();
            self.input.drain(self.input_cursor..next);
        }
    }
}

// ── Intervention Helpers ──────────────────────────────────────────────────────────

impl App {
    /// Returns `true` if there is an unresponded [`ChatMessage::Intervene`]
    /// in the message list.
    pub(crate) fn has_pending_intervene(&self) -> bool {
        self.messages.iter().rev().any(|msg| {
            matches!(
                msg,
                ChatMessage::Intervene {
                    responded: false,
                    ..
                }
            )
        })
    }

    /// Routes all key presses while an intervention prompt is active.
    ///
    /// Two sub-modes:
    /// - **Navigation** (`intervene_text_mode == false`): ↑↓ to move
    ///   highlight, Enter to select, Esc to cancel, first-char to jump.
    /// - **Text input** (`intervene_text_mode == true`): typing custom
    ///   text for the "…"-suffixed option. Enter submits, Esc goes back.
    fn handle_intervene_key(&mut self, key: KeyEvent) -> Option<TuiCommand> {
        // ── Text-input sub-mode ──────────────────────────────────
        if self.intervene_text_mode {
            return self.handle_intervene_text_key(key);
        }

        // ── Navigation sub-mode ──────────────────────────────────
        // Lazy-init the selection to the first option.
        let (options_len, _responded) = self.intervene_state();
        if self.intervene_selection.is_none() || self.intervene_selection.unwrap() >= options_len {
            self.intervene_selection = Some(0);
        }

        match key.code {
            KeyCode::Up => {
                if let Some(sel) = self.intervene_selection.as_mut() {
                    *sel = sel.saturating_sub(1);
                }
                None
            }
            KeyCode::Down => {
                if let Some(sel) = self.intervene_selection.as_mut() {
                    *sel = (*sel + 1).min(options_len.saturating_sub(1));
                }
                None
            }
            KeyCode::Enter => {
                let sel = self.intervene_selection.unwrap_or(0);
                let options: Vec<String> = self
                    .messages
                    .iter()
                    .rev()
                    .find_map(|msg| match msg {
                        ChatMessage::Intervene {
                            responded: false,
                            options,
                            ..
                        } => Some(options.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();

                let chosen_label = options.get(sel).cloned().unwrap_or_default();

                if chosen_label.ends_with('…') {
                    // Enter text-input sub-mode instead of submitting.
                    self.enter_intervene_text_mode();
                    None
                } else {
                    // Regular option — confirm immediately.
                    self.complete_intervene(Some(sel), None)
                }
            }
            KeyCode::Esc => {
                // Cancel the intervention.
                self.complete_intervene(None, None)
            }
            KeyCode::Char(c) => {
                // Navigate to the first option whose label starts with
                // this character (case-insensitive). Does NOT auto-confirm.
                let c_lower = c.to_ascii_lowercase();
                let options: Vec<String> = self
                    .messages
                    .iter()
                    .rev()
                    .find_map(|msg| match msg {
                        ChatMessage::Intervene {
                            responded: false,
                            options,
                            ..
                        } => Some(options.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                for (i, opt) in options.iter().enumerate() {
                    if opt.to_ascii_lowercase().starts_with(c_lower) {
                        self.intervene_selection = Some(i);
                        break;
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Handles keys while the user is typing custom text for an
    /// "Other…" option.
    fn handle_intervene_text_key(&mut self, key: KeyEvent) -> Option<TuiCommand> {
        match key.code {
            KeyCode::Enter => {
                // Submit the custom text and restore the original input.
                let text = self.input.clone();
                self.exit_intervene_text_mode();
                let sel = self.intervene_selection.unwrap_or(0);
                let custom = if text.is_empty() { None } else { Some(text) };
                self.complete_intervene(Some(sel), custom)
            }
            KeyCode::Esc => {
                // Cancel text mode — go back to navigation.
                self.exit_intervene_text_mode();
                None
            }
            KeyCode::Backspace => {
                if self.input_cursor > 0 {
                    let prev = self.prev_char_boundary();
                    self.input.remove(prev);
                    self.input_cursor = prev;
                }
                None
            }
            KeyCode::Delete => {
                if self.input_cursor < self.input.len() {
                    self.delete_at_cursor();
                }
                None
            }
            KeyCode::Left => {
                if self.input_cursor > 0 {
                    self.input_cursor = self.prev_char_boundary();
                }
                None
            }
            KeyCode::Right => {
                if self.input_cursor < self.input.len() {
                    self.input_cursor = self.next_char_boundary();
                }
                None
            }
            KeyCode::Home => {
                self.input_cursor = 0;
                None
            }
            KeyCode::End => {
                self.input_cursor = self.input.len();
                None
            }
            KeyCode::Char(c) => {
                self.input.insert(self.input_cursor, c);
                self.input_cursor += c.len_utf8();
                None
            }
            _ => None,
        }
    }

    /// Saves the current input buffer and enters custom-text mode for
    /// the "Other…" option.
    fn enter_intervene_text_mode(&mut self) {
        self.intervene_saved_input = self.input.clone();
        self.intervene_saved_cursor = self.input_cursor;
        self.input.clear();
        self.input_cursor = 0;
        self.intervene_text_mode = true;
    }

    /// Restores the input buffer from before custom-text mode and
    /// returns to navigation mode.
    fn exit_intervene_text_mode(&mut self) {
        self.input = self.intervene_saved_input.clone();
        self.input_cursor = self.intervene_saved_cursor;
        self.intervene_saved_input.clear();
        self.intervene_saved_cursor = 0;
        self.intervene_text_mode = false;
    }

    /// Returns `(options_len, has_been_responded)` for the pending intervention.
    fn intervene_state(&self) -> (usize, bool) {
        self.messages
            .iter()
            .rev()
            .find_map(|msg| match msg {
                ChatMessage::Intervene {
                    options, responded, ..
                } => Some((options.len(), *responded)),
                _ => None,
            })
            .unwrap_or((0, true))
    }

    /// Marks the last unresponded intervention as completed and returns
    /// the [`TuiCommand::InterventionResponse`].
    fn complete_intervene(
        &mut self,
        chosen: Option<usize>,
        custom_text: Option<String>,
    ) -> Option<TuiCommand> {
        self.intervene_selection = None;
        self.intervene_text_mode = false;
        let response = engine::InterventionResponse {
            chosen,
            custom_text,
        };
        for msg in self.messages.iter_mut().rev() {
            if let ChatMessage::Intervene {
                request_id,
                responded,
                chosen,
                custom_text,
                ..
            } = msg
                && !*responded
            {
                *responded = true;
                *chosen = response.chosen;
                *custom_text = response.custom_text.clone();
                return Some(TuiCommand::InterventionResponse {
                    request_id: request_id.clone(),
                    response,
                });
            }
        }
        None
    }
}
