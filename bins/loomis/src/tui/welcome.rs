//! # Welcome — startup banner (ASCII logo + mascot)
//!
//! Pure presentation module backing [`ChatMessage::Welcome`]. The banner is
//! the first message seeded into the chat history by [`super::app::App::new`]
//! and stays in the scrollback like Claude Code's startup banner — no
//! full-screen splash, no animation, the user can type immediately.
//!
//! The mascot is 小织 (Xiǎo Zhī, "Little Weaver"), a weaver bird:
//! weaver birds weave elaborate nests, echoing Loomis (loom) weaving code.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::theme;

// ── ASCII Art ─────────────────────────────────────────────────────────────────

/// Full logo — 6 lines × 64 columns. Used when the chat area is wide enough.
pub const LOGO_FULL: &[&str] = &[
    "██╗      ██████╗  ██████╗ ███╗   ███╗██╗███████╗",
    "██║     ██╔═══██╗██╔═══██╗████╗ ████║██║██╔════╝",
    "██║     ██║   ██║██║   ██║██╔████╔██║██║███████╗",
    "██║     ██║   ██║██║   ██║██║╚██╔╝██║██║╚════██║",
    "███████╗╚██████╔╝╚██████╔╝██║ ╚═╝ ██║██║███████║",
    "╚══════╝ ╚═════╝  ╚═════╝ ╚═╝     ╚═╝╚═╝╚══════╝",
];

/// Compact logo — 3 lines × 16 columns. Fallback for narrow terminals.
pub const LOGO_COMPACT: &[&str] = &["╦  ╔═╗╔═╗╔╦╗╦╔═╗", "║  ║ ║║ ║║║║║╚═╗", "╩═╝╚═╝╚═╝╩ ╩╩╚═╝"];

/// 小织 the weaver bird — 3 lines × 9 columns (rows pre-padded to equal width).
pub const MASCOT: &[&str] = &[" __      ", "<(o )___ ", " (.__)_/ "];

/// Chat-area width below which the compact logo is used.
pub const COMPACT_THRESHOLD: u16 = 72;

/// Indent (in columns) applied to every banner line.
const INDENT: &str = "    ";

/// Column (within the composed line, before `INDENT`) where the info text
/// starts next to the mascot — mascot width (9) + 5 spaces of gap.
const INFO_COL: usize = 14;

/// Mascot row (0-based) carrying the tagline — the crest line.
const TAGLINE_ROW: usize = 0;
/// Mascot row carrying the model name — the face line.
const MODEL_ROW: usize = 1;
/// Mascot row carrying the workspace path — the body line.
const WORKSPACE_ROW: usize = 2;

const TAGLINE: &str = "weaving code, together.";
const TIPS: &str = "/help commands · /plan plan mode · !cmd shell";

// ── Rendering ─────────────────────────────────────────────────────────────────

/// Builds the styled banner lines for [`ChatMessage::Welcome`].
///
/// Layout: logo (6 or 3 lines) + blank + mascot block with the info column
/// (3 lines) + blank + tips = 12 lines full / 9 lines compact.
pub fn render(model: &str, workspace: &str, area_width: u16) -> Vec<Line<'static>> {
    let logo = if area_width >= COMPACT_THRESHOLD {
        LOGO_FULL
    } else {
        LOGO_COMPACT
    };

    let logo_style = Style::default()
        .fg(theme::HEADING)
        .add_modifier(Modifier::BOLD);
    let mascot_style = Style::default()
        .fg(theme::WARNING)
        .add_modifier(Modifier::BOLD);
    let tagline_style = theme::hint_style().add_modifier(Modifier::ITALIC);
    let label_style = Style::default().fg(theme::TEXT_SECONDARY);
    let value_style = Style::default().fg(theme::TEXT_PRIMARY);

    let mut lines: Vec<Line<'static>> = Vec::new();

    // ── Logo ──
    for row in logo {
        lines.push(Line::from(Span::styled(
            format!("{INDENT}{row}"),
            logo_style,
        )));
    }
    lines.push(Line::default());

    // ── Mascot + info column ──
    // Long workspace paths are truncated to keep the banner inside the area.
    let info_budget = (area_width as usize).saturating_sub(INDENT.len() + INFO_COL + 12);
    let workspace = truncate_to_width(workspace, info_budget.max(16));

    for (row, art) in MASCOT.iter().enumerate() {
        let mut spans = vec![Span::styled(format!("{INDENT}{art}"), mascot_style)];
        match row {
            TAGLINE_ROW => {
                spans.push(gap(art));
                spans.push(Span::styled(TAGLINE.to_string(), tagline_style));
            }
            MODEL_ROW => {
                spans.push(gap(art));
                spans.push(Span::styled("model       ".to_string(), label_style));
                spans.push(Span::styled(model.to_string(), value_style));
            }
            WORKSPACE_ROW => {
                spans.push(gap(art));
                spans.push(Span::styled("workspace   ".to_string(), label_style));
                spans.push(Span::styled(workspace.clone(), value_style));
            }
            _ => {}
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::default());

    // ── Tips ──
    lines.push(Line::from(Span::styled(
        format!("{INDENT}{TIPS}"),
        theme::hint_style(),
    )));

    lines
}

/// Number of visual lines the banner occupies at the given chat width.
/// Mirrors [`render`]'s layout so [`super::ui::estimate_lines`] stays in sync.
pub fn line_count(area_width: u16) -> usize {
    let logo_lines = if area_width >= COMPACT_THRESHOLD {
        LOGO_FULL.len()
    } else {
        LOGO_COMPACT.len()
    };
    logo_lines + 1 + MASCOT.len() + 1 + 1
}

/// Spaces between the mascot's right edge and the info column.
fn gap(art: &str) -> Span<'static> {
    " ".repeat(INFO_COL.saturating_sub(UnicodeWidthStr::width(art)))
        .into()
}

/// Truncates `text` to `max_width` display columns, appending `…` when cut.
fn truncate_to_width(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + w > max_width.saturating_sub(1) {
            break;
        }
        out.push(ch);
        width += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_count_matches_render() {
        for width in [40, 71, 72, 80, 120] {
            let rendered = render("deepseek-chat", r"c:\workspace\loomis", width);
            assert_eq!(
                rendered.len(),
                line_count(width),
                "render/line_count mismatch at width {width}"
            );
        }
    }

    #[test]
    fn mascot_rows_are_equal_display_width() {
        for row in MASCOT {
            assert_eq!(UnicodeWidthStr::width(*row), 9, "row {row:?} not 9 wide");
        }
    }

    #[test]
    fn visual_preview() {
        for width in [100, 60] {
            println!("── width {width} ──");
            for line in render(
                "deepseek-chat",
                r"c:\Users\Administrator\RustroverProjects\loomis",
                width,
            ) {
                let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                println!("{s}");
            }
        }
    }

    #[test]
    fn compact_logo_used_below_threshold() {
        let lines = render("m", "w", COMPACT_THRESHOLD - 1);
        let first = lines[0].spans[0].content.as_ref();
        assert!(first.contains('╔'), "expected compact logo, got {first:?}");
    }
}
