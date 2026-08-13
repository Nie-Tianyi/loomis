//! # Event Loop
//!
//! Bridges the synchronous ratatui render loop with the async agent driver
//! in loomis-core. The main thread runs the TUI (render + input) and sends
//! [`RuntimeCommand`]s to the driver task, which manages the agent
//! lifecycle (spawn, cancel, clear).
//!
//! ## Channel topology
//!
//! ```text
//! TUI thread                            Driver task (loomis-core, tokio::spawn)
//! ─────────                            ────────────────────────
//! Runtime::send ── RuntimeCommand ───→ cmd_rx
//! agent_rx ←────── AgentEvent ──────── agent_tx (Agent loop + SandboxHook + user cmds)
//! ```

use std::io;
use std::path::PathBuf;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use tokio::sync::mpsc::UnboundedReceiver;

use loomis_core::{AgentEvent, Runtime, RuntimeCommand};

use super::app::App;

// ── Entry Point ──────────────────────────────────────────────────────────────────

/// Initialises the TUI and runs the event loop until the user exits.
///
/// This function is **synchronous** — it blocks the calling thread until
/// the user types `/exit` or presses Ctrl+C/D. The caller must already be
/// inside a tokio runtime (e.g. via `#[tokio::main]`).
pub fn run(runtime: Runtime, workspace_root: PathBuf, model: &str) -> io::Result<()> {
    // ── Start the agent driver task ────────────────────────────────
    let agent_rx = runtime.spawn();

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
    // The fresh-session thread-marker reset happens in `Runtime::build`.

    // ── App state ────────────────────────────────────────────────────
    // Cheap per-frame read handles from the runtime (all Arc clones).
    let ui = runtime.ui();
    let mut app = App::new(model, workspace_root, ui, runtime.clone());

    // ── Event loop ───────────────────────────────────────────────────
    let result = run_event_loop(&mut terminal, &mut app, agent_rx, &runtime);

    // ── Cleanup ──────────────────────────────────────────────────────
    runtime.shutdown();
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
    runtime: &Runtime,
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
                    if matches!(cmd, RuntimeCommand::Shutdown) {
                        return Ok(());
                    }
                    runtime.send(cmd);
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
fn dispatch_terminal_event(app: &mut App, event: Event) -> Option<RuntimeCommand> {
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

// ── Tests ──────────────────────────────────────────────────────────────────────

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
