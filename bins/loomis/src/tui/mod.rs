//! # TUI — Terminal User Interface
//!
//! A ratatui-based chat interface for Agent Oxide, inspired by Claude Code's
//! terminal UX: scrollable conversation history, real-time streaming tokens,
//! styled tool calls, and an interactive input area.
//!
//! ## Architecture
//!
//! ```text
//! main.rs ──→ tui::run() ──→ event_loop()
//!               │                │
//!               │                ├─ poll crossterm keys (50ms timeout)
//!               │                ├─ drain agent events via try_recv
//!               │                └─ render frame via ratatui
//!               │
//!               └── loomis_core::Runtime (driver task, tokio::spawn)
//!                      │
//!                      └── loop { recv(RuntimeCommand) → run agent / cancel / clear }
//! ```
//!
//! The agent itself — tools, hooks, driver, event stream — lives in
//! [`loomis_core`], reached only through the [`Runtime`](loomis_core::Runtime)
//! façade. This module is pure presentation.
//!
//! ## Modules
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`app`] | `App` state machine + event application |
//! | [`messages`] | `ChatMessage`, `ThreadPicker`, `ToolCallState` types |
//! | [`input`] | Keyboard handling, slash commands, shell confirmation |
//! | [`keyboard`] | Platform-aware shortcut helpers (Cmd vs Ctrl) |
//! | [`paste`] | Pasted-content model: placeholders + expansion |
//! | [`ui`] | ratatui rendering: chat area, input area, status bar |
//! | [`event`] | Event loop, terminal lifecycle, runtime wiring |
//! | [`welcome`] | Startup banner: ASCII logo + mascot 小织 |

mod app;
mod event;
mod input;
mod keyboard;
mod markdown;
mod messages;
mod paste;
mod theme;
mod ui;
mod welcome;

pub use event::run;
