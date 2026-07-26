//! # Theme — semantic color & symbol palette
//!
//! Every visual constant used by the TUI lives here so that [`super::ui`]
//! and [`super::markdown`] never hardcode `Color::Rgb(...)` values or icon
//! literals inline. Changing the look of the app means editing this one
//! file (Nielsen #4: consistency and standards).
//!
//! Constants are grouped by purpose: semantic colors, status bar, borders,
//! overlays, markdown rendering, icons, and spinner animation frames.

use ratatui::style::{Color, Modifier, Style};

// ── Semantic Colors ─────────────────────────────────────────────────────────────

/// Primary accent — user prompts, input border (idle), links to actions.
pub const ACCENT: Color = Color::Cyan;
/// Warning / in-progress — running tools, reasoning, interventions, plan mode.
pub const WARNING: Color = Color::Yellow;
/// Success — completed tools, responded interventions, shell prompts.
pub const SUCCESS: Color = Color::Green;
/// Error — failures of any kind.
pub const ERROR: Color = Color::Red;
/// Informational — system messages, intervention borders.
pub const INFO: Color = Color::Magenta;

/// Primary text on the chat background.
pub const TEXT_PRIMARY: Color = Color::White;
/// Secondary text — timestamps, unselected options, placeholders.
pub const TEXT_SECONDARY: Color = Color::DarkGray;
/// Tertiary text — completion descriptions, status bar foreground.
pub const TEXT_DIM: Color = Color::Rgb(180, 190, 200);
/// Tool output preview text (brighter than secondary, dimmer than primary).
pub const TEXT_OUTPUT: Color = Color::Gray;
/// Cursor foreground when the cursor is rendered as an inverted block.
pub const CURSOR_FG: Color = Color::Black;

// ── Status Bar ──────────────────────────────────────────────────────────────────

pub const STATUS_BG: Color = Color::Rgb(30, 40, 50);
pub const STATUS_FG: Color = TEXT_DIM;
/// Accent while the agent is streaming (amber).
pub const STATUS_ACCENT_STREAMING: Color = Color::Rgb(255, 180, 50);
/// Accent while idle (green).
pub const STATUS_ACCENT_IDLE: Color = Color::Rgb(80, 200, 120);

// ── Borders ─────────────────────────────────────────────────────────────────────

/// Chat area border.
pub const BORDER: Color = Color::DarkGray;
/// Input border — idle.
pub const BORDER_INPUT: Color = ACCENT;
/// Input border — streaming / inject mode.
pub const BORDER_INJECT: Color = WARNING;
/// Input border — intervention pending.
pub const BORDER_CHOOSE: Color = INFO;
/// Input border — plan mode active.
pub const BORDER_PLAN: Color = WARNING;
/// Input border — shell command confirmation pending.
pub const BORDER_CONFIRM: Color = WARNING;

// ── Overlays & Selection ────────────────────────────────────────────────────────

/// Mouse text-selection highlight background (blue-gray).
pub const SELECTION_BG: Color = Color::Rgb(50, 60, 90);
/// Popup overlay background (thread picker, help, slash completion).
pub const OVERLAY_BG: Color = Color::Rgb(20, 25, 35);

/// Scrollbar thumb character color.
pub const SCROLLBAR_THUMB: Color = Color::DarkGray;
/// Scrollbar track color (rendered with DIM modifier).
pub const SCROLLBAR_TRACK: Color = Color::Rgb(60, 60, 60);

// ── Markdown ────────────────────────────────────────────────────────────────────

pub const HEADING: Color = Color::Rgb(100, 180, 255);
pub const HEADING2: Color = Color::Rgb(130, 200, 255);
pub const CODE_BG: Color = Color::Rgb(40, 44, 52);
pub const INLINE_CODE_BG: Color = Color::Rgb(55, 55, 65);
/// Code block / inline code text (warm gray).
pub const CODE_TEXT: Color = Color::Rgb(200, 200, 180);
pub const QUOTE_BORDER: Color = Color::Rgb(100, 140, 180);
pub const QUOTE_TEXT: Color = Color::Rgb(170, 180, 195);
pub const LINK: Color = Color::Rgb(80, 160, 220);
pub const RULE: Color = Color::Rgb(70, 70, 80);
pub const BULLET: Color = Color::Rgb(130, 160, 190);
pub const TABLE_BORDER: Color = Color::Rgb(80, 85, 95);
pub const TABLE_BODY: Color = Color::Rgb(220, 225, 235);
/// Thin row separator in large tables (rendered with DIM modifier).
pub const TABLE_SEPARATOR: Color = Color::Rgb(50, 55, 60);

// ── Icons ───────────────────────────────────────────────────────────────────────

/// Icons include their trailing space so they can be dropped into
/// `Span::styled(theme::ICON_*, ...)` without extra formatting.
pub const ICON_USER: &str = "> ";
pub const ICON_SHELL: &str = "$ ";
pub const ICON_SUCCESS: &str = "✓ ";
pub const ICON_REJECTED: &str = "⊘ ";
pub const ICON_ERROR: &str = "✗ ";
pub const ICON_INFO: &str = "ℹ ";
pub const ICON_INTERVENTION: &str = "⚡ ";
pub const ICON_SELECTED: &str = "▶ ";
pub const SCROLL_THUMB: &str = "█";
pub const SCROLL_TRACK: &str = "│";

// ── Spinner ─────────────────────────────────────────────────────────────────────

/// Braille-dot animation frames, cycled while the agent runs.
pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Minimum milliseconds between frame advances.
pub const SPINNER_INTERVAL_MS: u64 = 100;

// ── Shared Styles ───────────────────────────────────────────────────────────────

/// Dim timestamp style prepended to every chat message.
pub fn ts_style() -> Style {
    Style::default()
        .fg(TEXT_SECONDARY)
        .add_modifier(Modifier::DIM)
}

/// Placeholder / hint text style (empty-input hints, overlay footers).
pub fn hint_style() -> Style {
    Style::default()
        .fg(TEXT_SECONDARY)
        .add_modifier(Modifier::DIM)
}
