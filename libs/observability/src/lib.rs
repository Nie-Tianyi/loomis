//! Observability crate for the Loomis agent framework.
//!
//! Provides:
//!
//! - [`TraceEvent`] — granular lifecycle events (LLM calls, tool executions, …).
//! - [`TraceStore`] — dispatches events to the [`tracing`] infrastructure.
//! - [`RunMetrics`] — aggregated counters and timing data for the TUI status bar.

pub mod event;
pub mod store;

pub use event::TraceEvent;
pub use store::{RunMetrics, TraceStore};
