//! # Event Loop
//!
//! Bridges the synchronous ratatui render loop with the async tokio agent.
//! The main thread runs the TUI (render + input), while a background tokio
//! task manages the agent lifecycle (spawn, cancel, clear).
//!
//! ## Channel topology
//!
//! ```text
//! TUI thread                          Agent task (tokio::spawn)
//! ─────────                          ────────────────────────
//! cmd_tx ───────── TuiCommand ──────→ cmd_rx
//! agent_rx ←────── AgentEvent ─────── agent_tx (Agent loop + SandboxHook + user cmds)
//! ```

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::FutureExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use agent_oxide::deepseek::DeepSeekClient;
use agent_oxide::engine::{Agent, AgentEvent, CallOrigin};
use agent_oxide::memory::SharedMemory;
use agent_oxide::persistence::PersistenceConfig;
use agent_oxide::provider::{Message, Role};

use super::app::App;
use super::messages::TuiCommand;
use super::shell_exec::execute_shell_command;
use crate::app::AgentKit;
use crate::hooks::SYSPROMPT_MARKER;

// ── Entry Point ──────────────────────────────────────────────────────────────────

/// Initialises the TUI and runs the event loop until the user exits.
///
/// This function is **synchronous** — it blocks the calling thread until
/// the user types `/exit` or presses Ctrl+C/D. The caller must already be
/// inside a tokio runtime (e.g. via `#[tokio::main]`).
pub fn run(kit: AgentKit, workspace_root: PathBuf, model: &str) -> io::Result<()> {
    // ── Destructure the kit ─────────────────────────────────────
    let AgentKit {
        agent,
        memory,
        tool_names,
        model: _kit_model,
        agent_rx,
        agent_tx,
        response_router,
        pending_hints,
        persistence_config,
        todos,
        trace_store,
        plan_mode,
        plan_dir,
        skill_registry,
        active_skills,
        shell_filter,
    } = kit;

    // ── Create command channel ────────────────────────────────────
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<TuiCommand>();

    // ── Spawn agent handler ─────────────────────────────────────────
    tokio::spawn(agent_handler(
        Arc::new(agent),
        memory.clone(),
        cmd_rx,
        agent_tx,
        workspace_root.clone(),
        response_router,
        persistence_config.clone(),
    ));

    // ── Terminal setup ───────────────────────────────────────────────
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
        // Bracketed paste makes the terminal wrap pasted text in escape
        // markers, so a paste arrives as one Event::Paste instead of a
        // burst of keystrokes that would submit one message per line.
        crossterm::event::EnableBracketedPaste,
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;

    // NOTE: the process-wide panic hook (installed in main.rs) restores the
    // terminal and writes the panic to the log — no TUI-specific hook here.

    // Fresh session: reset the current-thread marker so PersistenceHook's
    // on_run_start treats the first query as a new conversation and
    // generates a title for it.  (A leftover name from a previous session
    // must not hijack the new conversation — on_run_finish would otherwise
    // overwrite the old thread file.)
    let _ = agent_oxide::persistence::write_current_thread_name(
        &persistence_config.default_thread_name,
        &workspace_root,
        &persistence_config,
    );

    // ── App state ────────────────────────────────────────────────────
    let mut app = App::new(
        model,
        memory,
        tool_names,
        todos,
        workspace_root,
        pending_hints,
        persistence_config,
        trace_store,
        plan_mode,
        plan_dir,
        skill_registry,
        active_skills,
        shell_filter,
    );

    // ── Event loop ───────────────────────────────────────────────────
    let result = run_event_loop(&mut terminal, &mut app, agent_rx, &cmd_tx);

    // ── Cleanup ──────────────────────────────────────────────────────
    let _ = cmd_tx.send(TuiCommand::Exit);
    terminal.show_cursor()?;
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        io::stdout(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste,
    )?;

    result
}

// ── Event Loop ───────────────────────────────────────────────────────────────────

/// The main TUI loop: poll input, drain agent events, render.
fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    agent_rx: UnboundedReceiver<AgentEvent>,
    cmd_tx: &UnboundedSender<TuiCommand>,
) -> io::Result<()> {
    let mut agent_rx = agent_rx;
    let mut pending_events: Vec<AgentEvent> = Vec::new();

    loop {
        // ── Advance spinner ──────────────────────────────────────────
        // The loop wakes at least every 50ms (poll timeout below), so the
        // spinner ticks without any extra timer machinery.
        let now = std::time::Instant::now();
        if app.streaming
            && now.duration_since(app.last_spinner_tick)
                >= std::time::Duration::from_millis(super::theme::SPINNER_INTERVAL_MS)
        {
            app.spinner_frame = (app.spinner_frame + 1) % super::theme::SPINNER_FRAMES.len();
            app.last_spinner_tick = now;
        }

        // ── Render ───────────────────────────────────────────────────
        terminal.draw(|frame| super::ui::draw(frame, app))?;

        // ── Poll terminal input ──────────────────────────────────────
        if crossterm::event::poll(std::time::Duration::from_millis(50))? {
            // Read the event that woke us, then drain everything already
            // queued behind it. A terminal-injected paste (right-click in
            // conhost / Windows Terminal, or a terminal-side Ctrl+V)
            // arrives as one instant burst of key events — coalescing the
            // batch is what lets us tell it apart from human typing. This
            // matters on Windows, where crossterm's backend never emits
            // Event::Paste (bracketed-paste parsing is Unix-only).
            let mut event_batch = vec![crossterm::event::read()?];
            while crossterm::event::poll(std::time::Duration::ZERO)? {
                event_batch.push(crossterm::event::read()?);
            }

            if looks_like_paste_burst(&event_batch) {
                // Large pastes can be split across pipe chunks; keep
                // absorbing events while they arrive within a brief
                // trailing window so one paste isn't cut in two.
                while crossterm::event::poll(std::time::Duration::from_millis(2))? {
                    event_batch.push(crossterm::event::read()?);
                    while crossterm::event::poll(std::time::Duration::ZERO)? {
                        event_batch.push(crossterm::event::read()?);
                    }
                }
                let pasted = paste_batch_to_text(&event_batch);
                tracing::info!(
                    batch_events = event_batch.len(),
                    pasted_len = pasted.len(),
                    pasted_newlines = pasted.chars().filter(|&c| c == '\n').count(),
                    pasted_cr = pasted.chars().filter(|&c| c == '\r').count(),
                    "paste burst detected",
                );
                app.handle_paste(&pasted);
            } else {
                for event in event_batch {
                    let Some(cmd) = dispatch_terminal_event(app, event) else {
                        continue;
                    };
                    if matches!(cmd, TuiCommand::Exit) {
                        return Ok(());
                    }
                    if cmd_tx.send(cmd).is_err() {
                        tracing::error!("Failed to send TuiCommand to agent handler");
                    }
                }
            }
        }

        // ── Drain agent events (single channel for everything) ─────
        while let Ok(event) = agent_rx.try_recv() {
            pending_events.push(event);
        }

        // Apply all agent events together
        for event in pending_events.drain(..) {
            app.apply_event(event);
        }

        // ── Quit signal ──────────────────────────────────────────────
        if app.should_quit {
            return Ok(());
        }
    }
}

// ── Terminal Event Dispatch ─────────────────────────────────────────────────────

/// Routes one terminal event to its handler, returning a command for the
/// agent task when the event triggers one.
fn dispatch_terminal_event(app: &mut App, event: Event) -> Option<TuiCommand> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat => {
            app.handle_key(key)
        }
        Event::Resize(..) => {
            // ratatui handles resize in terminal.draw(), but we reset
            // scroll so the viewport doesn't end up in a weird state.
            app.scroll_offset = 0;
            app.auto_scroll = true;
            // Re-wrapping invalidates the line/column indices an
            // in-progress selection points at — drop it.
            app.clear_selection();
            None
        }
        Event::Mouse(mouse_event) => {
            app.handle_mouse_event(&mouse_event);
            None
        }
        Event::Paste(text) => {
            // Bracketed paste — emitted by Unix terminals only.
            tracing::info!(
                paste_len = text.len(),
                paste_newlines = text.chars().filter(|&c| c == '\n').count(),
                paste_cr = text.chars().filter(|&c| c == '\r').count(),
                "Event::Paste received",
            );
            app.handle_paste(&text);
            None
        }
        _ => None,
    }
}

// ── Paste Burst Detection ────────────────────────────────────────────────────────

/// Minimum key presses in one event batch before it is considered a paste.
///
/// A human produces one keypress per poll window (occasionally two when
/// rolling between keys); a terminal-injected paste produces dozens in a
/// single instant.
const MIN_PASTE_BURST_KEYS: usize = 3;

/// Returns `true` when a batch of events read from a single poll window
/// looks like a terminal-injected paste rather than human typing:
/// several key presses, at least one of them Enter.
///
/// The Enter is the telltale sign — humans never press Enter bundled with
/// other keys in the same instant, so an Enter inside a multi-key burst
/// is a pasted newline, not a submission. Without this check, a multi-line
/// paste on Windows submits one message per line.
fn looks_like_paste_burst(event_batch: &[Event]) -> bool {
    let pressed_keys: Vec<&KeyEvent> = event_batch
        .iter()
        .filter_map(|event| match event {
            Event::Key(key)
                if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat =>
            {
                Some(key)
            }
            _ => None,
        })
        .collect();
    let contains_newline = pressed_keys.iter().any(|key| key.code == KeyCode::Enter);
    pressed_keys.len() >= MIN_PASTE_BURST_KEYS && contains_newline
}

/// Flattens a paste burst back into the pasted text: characters become
/// themselves, Enter becomes `'\n'`. Key releases and control combinations
/// are not paste content and are dropped.
fn paste_batch_to_text(event_batch: &[Event]) -> String {
    let mut pasted_text = String::new();
    for event in event_batch {
        let Event::Key(key) = event else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                pasted_text.push(c);
            }
            KeyCode::Enter => pasted_text.push('\n'),
            _ => {}
        }
    }
    pasted_text
}

// ── Agent Handler ────────────────────────────────────────────────────────────────

/// Background task that processes [`TuiCommand`]s and manages the agent lifecycle.
///
/// The agent is wrapped in an `Arc` so it can be shared into spawned tasks.
/// When a [`TuiCommand::RunAgent`] arrives, we push the user message to memory
/// and spawn a new tokio task that calls `Agent::run_with_events()`. The events
/// flow back to the TUI through `agent_tx`.
///
/// Cancellation is handled via `JoinHandle::abort()`. Since the agent's
/// own `run_streaming_loop` periodically `.await`s (network I/O), abort
/// takes effect quickly.
async fn agent_handler(
    agent: Arc<Agent<DeepSeekClient>>,
    memory: SharedMemory,
    mut cmd_rx: UnboundedReceiver<TuiCommand>,
    agent_tx: UnboundedSender<AgentEvent>,
    workspace_root: PathBuf,
    response_router: Arc<agent_oxide::engine::ResponseRouter>,
    persistence_config: PersistenceConfig,
) {
    let mut current_run: Option<tokio::task::JoinHandle<()>> = None;

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            TuiCommand::RunAgent(input) => {
                tracing::debug!(
                    input_len = input.chars().count(),
                    "RunAgent command received; spawning agent task",
                );

                // If a previous run is still active, cancel it.
                if let Some(h) = current_run.take() {
                    h.abort();
                }

                // Spawn the agent in a background task.
                // (`run_with_events` pushes the user message to memory internally)
                // Auto-save is handled by PersistenceHook::on_run_finish.
                let tx = agent_tx.clone();
                let agent = Arc::clone(&agent);

                let handle = tokio::spawn(async move {
                    let result =
                        std::panic::AssertUnwindSafe(agent.run_with_events(&input, tx.clone()))
                            .catch_unwind()
                            .await;
                    match result {
                        // Normal completion — PersistenceHook already saved the
                        // conversation in on_run_finish, and the agent loop
                        // already emitted RunCompleted/RunFailed + Done events.
                        Ok(Ok(_)) => {
                            tracing::debug!("Agent task finished normally");
                        }
                        Ok(Err(e)) => {
                            tracing::error!(error = %e, "Agent run failed");
                        }
                        // Panic inside the agent loop: tokio would otherwise
                        // swallow it silently. Log it and tell the TUI so the
                        // user isn't left in a stuck "streaming" state.
                        Err(payload) => {
                            let msg = agent_oxide::engine::panic_message(payload.as_ref());
                            tracing::error!(panic = %msg, "Agent task panicked");
                            let _ = tx.send(AgentEvent::RunFailed {
                                error: format!("Agent task panicked: {msg}"),
                            });
                            let _ = tx.send(AgentEvent::Done);
                        }
                    }
                });

                current_run = Some(handle);
            }

            TuiCommand::CancelGeneration => {
                tracing::debug!("CancelGeneration command received");
                if let Some(h) = current_run.take() {
                    h.abort();
                    // The agent task is killed immediately — no hooks can run.
                    // Emit cancellation events so the TUI shows proper feedback.
                    let _ = agent_tx.send(AgentEvent::Cancelled);
                    let _ = agent_tx.send(AgentEvent::Done);
                }
            }

            TuiCommand::ClearConversation => {
                tracing::debug!("ClearConversation command received");
                // Cancel any active generation.
                if let Some(h) = current_run.take() {
                    h.abort();
                }

                // Drain memory — preserve only the core system prompts
                // (identified by the [SYSPROMPT] marker).  Everything else
                // is regenerated on the next run: injector hooks
                // (SkillHook, ProfileHook, TodoListHook, PlanModeHook)
                // rebuild their marker messages from canonical state on
                // the first `on_llm_start`, and compaction summaries are
                // stale once the conversation is cleared.
                let mut mem = memory.write().expect("memory lock poisoned");
                let system_msgs: Vec<Message> = mem
                    .to_context_vec()
                    .into_iter()
                    .filter(|m| m.role == Role::System && m.content.starts_with(SYSPROMPT_MARKER))
                    .collect();
                let preserved = system_msgs.len();
                *mem = agent_oxide::memory::Memory::new();
                for msg in system_msgs {
                    mem.push(msg);
                }
                drop(mem); // release write lock before read-lock for save

                // Persist the cleared state.
                {
                    let mem = memory.read().expect("memory lock poisoned");
                    let name = agent_oxide::persistence::default_thread_name(
                        &workspace_root,
                        &persistence_config,
                    );
                    match agent_oxide::persistence::save_conversation(
                        &name,
                        &workspace_root,
                        &mem,
                        &persistence_config,
                    ) {
                        Ok(()) => {
                            tracing::debug!(preserved = preserved, "Cleared conversation persisted",)
                        }
                        Err(e) => tracing::error!(
                            name = %name,
                            error = %e,
                            "Failed to persist cleared conversation",
                        ),
                    }
                }
            }

            TuiCommand::InterventionResponse {
                request_id,
                response,
            } => {
                tracing::debug!(
                    request_id = %request_id.chars().take(12).collect::<String>(),
                    "InterventionResponse command received",
                );
                // Route the response to the correct requester
                // (SandboxHook, AskUserQuestionTool, …) via the
                // shared router.  The router removes the sender
                // from its map and delivers the response.
                response_router.route(&request_id, response);
            }

            TuiCommand::RunShell(command) => {
                tracing::debug!(
                    cmd = %command.chars().take(200).collect::<String>(),
                    "RunShell command received",
                );
                // Execute the shell command asynchronously — do NOT block
                // the agent handler or the TUI thread. The command runs
                // in a blocking thread; when it completes, output is
                // pushed to memory and sent to the TUI for display.
                //
                // Use unified ToolCall / ToolSuccessful events with User origin
                // instead of the old ShellRunning / ShellOutput events.
                let shell_id = format!(
                    "shell-{:x}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                );

                // Notify TUI that the command is starting.
                let _ = agent_tx.send(AgentEvent::ToolCall {
                    id: shell_id.clone(),
                    name: "shell".into(),
                    arguments: command.clone(),
                    origin: CallOrigin::User,
                });

                let tx = agent_tx.clone();
                let mem = memory.clone();
                let ws = workspace_root.clone();
                let cmd_for_blocking = command.clone();
                let sid = shell_id.clone();

                tokio::spawn(async move {
                    let output = tokio::task::spawn_blocking(move || {
                        execute_shell_command(&cmd_for_blocking, &ws)
                    })
                    .await
                    .unwrap_or_else(|e| format!("Task panicked: {e}"));

                    // Push into shared memory so the LLM sees it
                    {
                        let mut mem = mem.write().expect("memory lock poisoned");
                        mem.push(Message::new(
                            Role::User,
                            format!(
                                "User ran shell command: `{}`\n\nOutput:\n{}",
                                command, output
                            ),
                        ));
                    }

                    // Send result to TUI for display
                    let _ = tx.send(AgentEvent::ToolSuccessful {
                        id: sid,
                        name: "shell".into(),
                        output,
                    });
                });
            }

            TuiCommand::Exit => {
                tracing::debug!("Exit command received; saving conversation");
                // Save conversation before exiting.
                {
                    let mem = memory.read().expect("memory lock poisoned");
                    let name = agent_oxide::persistence::default_thread_name(
                        &workspace_root,
                        &persistence_config,
                    );
                    if let Err(e) = agent_oxide::persistence::save_conversation(
                        &name,
                        &workspace_root,
                        &mem,
                        &persistence_config,
                    ) {
                        tracing::error!(
                            name = %name,
                            error = %e,
                            "Failed to save conversation on exit",
                        );
                    }
                }

                if let Some(h) = current_run.take() {
                    h.abort();
                }
                break;
            }
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a key event with no modifiers and the given kind.
    fn key(code: KeyCode, kind: KeyEventKind) -> Event {
        Event::Key(KeyEvent::new_with_kind(code, KeyModifiers::NONE, kind))
    }

    /// Builds the event sequence one typed/pasted character produces on
    /// Windows: a Press followed by a Release.
    fn keystroke(code: KeyCode) -> Vec<Event> {
        vec![
            key(code, KeyEventKind::Press),
            key(code, KeyEventKind::Release),
        ]
    }

    #[test]
    fn test_multi_line_paste_burst_is_detected() {
        // Pasting "a\nb": three presses (a, Enter, b) interleaved with
        // releases — exactly what conhost/Windows Terminal injects.
        let mut batch = keystroke(KeyCode::Char('a'));
        batch.extend(keystroke(KeyCode::Enter));
        batch.extend(keystroke(KeyCode::Char('b')));

        assert!(looks_like_paste_burst(&batch));
        assert_eq!(paste_batch_to_text(&batch), "a\nb");
    }

    #[test]
    fn test_lone_enter_is_not_a_paste() {
        // A human pressing Enter: below the burst threshold, so it stays
        // a normal submission.
        let batch = keystroke(KeyCode::Enter);
        assert!(!looks_like_paste_burst(&batch));
    }

    #[test]
    fn test_fast_typist_rollover_is_not_a_paste() {
        // Two keys rolling over into one poll window (Enter + next char)
        // must not be mistaken for a paste — humans do this constantly.
        let mut batch = keystroke(KeyCode::Enter);
        batch.extend(keystroke(KeyCode::Char('x')));
        assert!(!looks_like_paste_burst(&batch));
    }

    #[test]
    fn test_single_line_keystroke_burst_is_not_a_paste() {
        // A terminal-injected single-line paste has many presses but no
        // Enter — dispatch normally; the chars land in the input either way.
        let batch: Vec<Event> = "hello"
            .chars()
            .flat_map(|c| keystroke(KeyCode::Char(c)))
            .collect();
        assert!(!looks_like_paste_burst(&batch));
    }

    #[test]
    fn test_paste_text_skips_control_chars_and_releases() {
        let mut batch = vec![Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        ))];
        batch.extend(keystroke(KeyCode::Char('x')));
        batch.extend(keystroke(KeyCode::Enter));

        assert!(looks_like_paste_burst(&batch));
        // Ctrl+C is a shortcut, not text; only 'x' and the newline survive.
        assert_eq!(paste_batch_to_text(&batch), "x\n");
    }
}
