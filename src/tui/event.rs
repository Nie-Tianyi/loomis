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
//! agent_rx ←────── AgentEvent ─────── agent_tx
//! ```

use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, KeyEventKind};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::core::agent::{Agent, AgentEvent};
use crate::core::client::{Message, Role};
use crate::memory::{Memory, SharedMemory};

use super::app::{App, TuiCommand};

// ── Entry Point ──────────────────────────────────────────────────────────────────

/// Initialises the TUI and runs the event loop until the user exits.
///
/// This function is **synchronous** — it blocks the calling thread until
/// the user types `/exit` or presses Ctrl+C/D. The caller must already be
/// inside a tokio runtime (e.g. via `#[tokio::main]`).
///
/// # Parameters
///
/// - `agent` — the configured [`Agent`] (moved into a background task).
/// - `memory` — shared conversation history.
/// - `tool_names` — cached list of tool names for the `/tools` command.
/// - `model` — model name for the status bar.
pub fn run(
    agent: Agent,
    memory: SharedMemory,
    tool_names: Vec<String>,
    model: &str,
) -> io::Result<()> {
    // ── Create channels ──────────────────────────────────────────────
    let (agent_tx, agent_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<TuiCommand>();

    // ── Spawn agent handler ─────────────────────────────────────────
    tokio::spawn(agent_handler(
        Arc::new(agent),
        memory.clone(),
        cmd_rx,
        agent_tx,
    ));

    // ── Terminal setup ───────────────────────────────────────────────
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;

    // Install panic hook to restore terminal on crash
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ =
            crossterm::execute!(io::stdout(), crossterm::terminal::LeaveAlternateScreen);
        prev_hook(info);
    }));

    // ── App state ────────────────────────────────────────────────────
    let mut app = App::new(model, memory, tool_names);

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
    )?;

    // Restore previous panic hook
    let _ = std::panic::take_hook();

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
    // We need to poll agent_rx without holding a mutable borrow on app
    // (terminal.draw borrows app immutably). So we collect events first,
    // then apply them.
    let mut agent_rx = agent_rx;
    let mut pending_events: Vec<AgentEvent> = Vec::new();

    loop {
        // ── Render ───────────────────────────────────────────────────
        terminal.draw(|frame| super::ui::draw(frame, app))?;

        // ── Poll keyboard ────────────────────────────────────────────
        if crossterm::event::poll(Duration::from_millis(50))? {
            match crossterm::event::read()? {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press
                        || key.kind == KeyEventKind::Repeat =>
                {
                    if let Some(cmd) = app.handle_key(key) {
                        match cmd {
                            TuiCommand::Exit => {
                                return Ok(());
                            }
                            cmd => {
                                let _ = cmd_tx.send(cmd);
                            }
                        }
                    }
                }
                Event::Resize(..) => {
                    // ratatui handles resize in terminal.draw(),
                    // but we reset scroll so the viewport doesn't end up
                    // in a weird state.
                    app.scroll_offset = 0;
                    app.auto_scroll = true;
                }
                _ => {}
            }
        }

        // ── Drain agent events ───────────────────────────────────────
        while let Ok(event) = agent_rx.try_recv() {
            pending_events.push(event);
        }

        // Apply all events together
        for event in pending_events.drain(..) {
            app.apply_event(event);
        }

        // ── Quit signal ──────────────────────────────────────────────
        if app.should_quit {
            return Ok(());
        }
    }
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
    agent: Arc<Agent>,
    memory: SharedMemory,
    mut cmd_rx: UnboundedReceiver<TuiCommand>,
    agent_tx: UnboundedSender<AgentEvent>,
) {
    let mut current_run: Option<tokio::task::JoinHandle<()>> = None;

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            TuiCommand::RunAgent(input) => {
                // If a previous run is still active, cancel it.
                if let Some(h) = current_run.take() {
                    h.abort();
                }

                // Push user message to shared memory.
                {
                    let mut mem = memory.write().unwrap();
                    mem.push(Message::new(Role::User, &input));
                }

                // Spawn the agent in a background task. We clone the
                // sender so the task owns its own — when the task
                // completes, its sender is dropped, freeing the channel
                // for the next run.
                let tx = agent_tx.clone();
                let agent = Arc::clone(&agent);

                let handle = tokio::spawn(async move {
                    match agent.run_with_events(tx.clone()).await {
                        Ok(_content) => {
                            // Agent sends Done internally on success.
                        }
                        Err(e) => {
                            let _ = tx.send(AgentEvent::Token(format!(
                                "\n✗ Error: {e}\n"
                            )));
                            let _ = tx.send(AgentEvent::Done);
                        }
                    }
                });

                current_run = Some(handle);
            }

            TuiCommand::CancelGeneration => {
                if let Some(h) = current_run.take() {
                    h.abort();
                }
                // Send cancellation notice to the TUI.
                let _ = agent_tx.send(AgentEvent::Token(
                    "\n[Cancelled]\n".to_string(),
                ));
                let _ = agent_tx.send(AgentEvent::Done);
            }

            TuiCommand::ClearConversation => {
                // Cancel any active generation.
                if let Some(h) = current_run.take() {
                    h.abort();
                }

                // Drain memory — preserve only System messages.
                let mut mem = memory.write().unwrap();
                let system_msgs: Vec<Message> = mem
                    .to_context_vec()
                    .into_iter()
                    .filter(|m| m.role == Role::System)
                    .collect();
                *mem = Memory::new();
                for msg in system_msgs {
                    mem.push(msg);
                }
            }

            TuiCommand::Exit => {
                if let Some(h) = current_run.take() {
                    h.abort();
                }
                break;
            }
        }
    }
}
