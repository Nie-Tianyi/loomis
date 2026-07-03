//! # Rendering
//!
//! All ratatui drawing for the three-panel layout: chat area, input area,
//! and status bar.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, ChatMessage, ToolCallState};

// ── Layout ───────────────────────────────────────────────────────────────────────

/// Entry point called from the event loop on every frame.
///
/// Splits the terminal into three vertical regions and delegates to
/// [`draw_chat`], [`draw_input`], and [`draw_status`].
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Rebuild line counts for accurate scrolling computation
    app.rebuild_line_counts(area.width);

    let layout = Layout::vertical([
        Constraint::Fill(1),      // chat
        Constraint::Length(3),    // input
        Constraint::Length(1),    // status bar
    ])
    .split(area);

    draw_chat(frame, layout[0], app);
    draw_input(frame, layout[1], app);
    draw_status(frame, layout[2], app);

    // Place the hardware cursor inside the input area.
    frame.set_cursor_position((
        layout[1].x + 2 + app.cursor_column(),
        layout[1].y + 1,
    ));
}

// ── Chat Area ────────────────────────────────────────────────────────────────────

/// Renders the scrollable conversation history.
fn draw_chat(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Chat ")
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    let visible_height = inner.height.max(1) as usize;

    // Convert messages → styled lines
    let all_lines: Vec<Line<'_>> = app
        .messages
        .iter()
        .flat_map(|msg| message_to_lines(msg, inner.width))
        .collect();

    let total_lines = all_lines.len();

    // Compute scroll offset: 0 = show last lines, > 0 = scroll up
    let max_scroll = total_lines.saturating_sub(visible_height);
    let scroll = (max_scroll.saturating_sub(app.scroll_offset)).min(max_scroll) as u16;

    let paragraph = Paragraph::new(Text::from(all_lines))
        .block(block)
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

/// Converts one [`ChatMessage`] into styled ratatui [`Line`]s.
///
/// Lines are **not** pre-wrapped — ratatui's `Paragraph` handles
/// word-wrap automatically via `.wrap()`. We use `area_width` to
/// estimate multi-line spans for the scroll offset calculation.
fn message_to_lines(msg: &ChatMessage, area_width: u16) -> Vec<Line<'_>> {
    match msg {
        ChatMessage::User { content } => {
            vec![Line::from(vec![
                Span::styled(
                    "> ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(content.as_str(), Style::default().fg(Color::White)),
            ])]
        }

        ChatMessage::Assistant { content } => content
            .lines()
            .map(|line| Line::from(Span::raw(line.to_string())))
            .collect(),

        ChatMessage::Reasoning { content } => content
            .lines()
            .map(|line| {
                Line::from(Span::styled(
                    line.to_string(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::DIM)
                        .add_modifier(Modifier::ITALIC),
                ))
            })
            .collect(),

        ChatMessage::ToolCall {
            name,
            args,
            state,
            ..
        } => {
            let mut lines = Vec::new();

            match state {
                ToolCallState::Running => {
                    // Show the tool being called
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            "🔧 ",
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            name.as_str(),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" "),
                        // Truncate args for display (still streaming)
                        Span::styled(
                            truncate_args(args, area_width),
                            Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
                        ),
                    ]));
                }

                ToolCallState::Complete(output) => {
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            "✓ ",
                            Style::default().fg(Color::Green),
                        ),
                        Span::styled(
                            name.as_str(),
                            Style::default()
                                .fg(Color::Green)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" → "),
                        Span::styled(
                            truncate_output(output, area_width),
                            Style::default().fg(Color::Gray),
                        ),
                    ]));
                }
            }

            lines
        }

        ChatMessage::System { content } => content
            .lines()
            .map(|line| {
                Line::from(vec![
                    Span::styled(
                        "  ℹ ",
                        Style::default()
                            .fg(Color::Magenta)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(line.to_string()),
                ])
            })
            .collect(),

        ChatMessage::Error { content } => content
            .lines()
            .map(|line| {
                Line::from(Span::styled(
                    line.to_string(),
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::BOLD),
                ))
            })
            .collect(),
    }
}

/// Truncates streaming JSON args for compact inline display.
fn truncate_args(args: &str, width: u16) -> String {
    let max = (width as usize).saturating_sub(8).max(10);
    let one_line = args.replace('\n', " ");
    if one_line.len() <= max {
        one_line
    } else {
        let end = one_line.floor_char_boundary(max);
        format!("{}…", &one_line[..end])
    }
}

/// Truncates tool output for compact inline display.
fn truncate_output(output: &str, width: u16) -> String {
    let max = (width as usize).saturating_sub(14).max(20);
    let one_line = output.replace('\n', " ");
    if one_line.len() <= max {
        one_line
    } else {
        let end = one_line.floor_char_boundary(max);
        format!("{}…", &one_line[..end])
    }
}

// ── Input Area ───────────────────────────────────────────────────────────────────

/// Renders the text input area with a border and cursor.
fn draw_input(frame: &mut Frame, area: Rect, app: &App) {
    let style = if app.streaming {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Cyan)
    };

    let title = if app.streaming {
        " Input (streaming…) "
    } else {
        " Input "
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(style);

    // Build the input line with cursor highlighting
    let cursor_style = if app.streaming {
        Style::default()
            .bg(Color::DarkGray)
            .fg(Color::White)
    } else {
        Style::default()
            .bg(Color::White)
            .fg(Color::Black)
    };

    let display_line = build_input_line(app, cursor_style);

    // Show a hint when the input is empty
    let lines = if app.input.is_empty() && !app.streaming {
        let hint = vec![
            Line::from(Span::raw("")),
            Line::from(Span::styled(
                " Type a message and press Enter. /help for commands.",
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            )),
        ];
        let p = Paragraph::new(hint).block(block);
        frame.render_widget(p, area);
        return;
    } else if app.streaming && app.input.is_empty() {
        vec![
            display_line,
            Line::from(Span::styled(
                " Generating… press Esc to cancel.",
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            )),
        ]
    } else {
        vec![display_line]
    };

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

/// Builds the styled input line: `"> "` prompt + text with cursor highlight.
fn build_input_line(app: &App, cursor_style: Style) -> Line<'_> {
    let mut spans: Vec<Span<'_>> = Vec::new();

    spans.push(Span::styled(
        "> ",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));

    let input = &app.input;
    let cursor = app.input_cursor;

    // Before cursor
    if cursor > 0 {
        spans.push(Span::raw(&input[..cursor]));
    }

    // Cursor character
    if cursor < input.len() {
        let ch = input[cursor..]
            .chars()
            .next()
            .unwrap_or(' ');
        spans.push(Span::styled(ch.to_string(), cursor_style));
        // After cursor
        let after_start = cursor + ch.len_utf8();
        if after_start < input.len() {
            spans.push(Span::raw(&input[after_start..]));
        }
    } else {
        // Cursor at end — show a space
        spans.push(Span::styled(" ", cursor_style));
    }

    Line::from(spans)
}

// ── Status Bar ───────────────────────────────────────────────────────────────────

/// Renders the single-line status bar at the bottom.
fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let (left, right) = build_status_content(app);

    // Fill gap between left and right
    let left_width = left.len();
    let right_width = right.len();
    let total_space = area.width as usize;

    let gap = if left_width + right_width < total_space {
        total_space - left_width - right_width
    } else {
        1
    };

    let line = Line::from(vec![
        Span::styled(
            left,
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
        Span::styled(
            " ".repeat(gap),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
        Span::styled(
            right,
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
    ]);

    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}

/// Builds the left and right portions of the status bar.
fn build_status_content(app: &App) -> (String, String) {
    let model = &app.model;
    let msgs = app.messages.len();

    let left = format!(" {model} | {} msgs ", msgs);

    let streaming_indicator = if app.streaming {
        "⚡ streaming"
    } else {
        "idle"
    };

    let scroll_info = if app.scroll_offset > 0 {
        format!(" | scrolled ↑{}", app.scroll_offset)
    } else {
        String::new()
    };

    let right = format!("{streaming_indicator}{scroll_info}  ");

    (left, right)
}

// ── Line Count Estimation ────────────────────────────────────────────────────────

impl App {
    /// Rebuilds the per-message line-count cache for scroll computation.
    ///
    /// Called once per frame from [`draw`]. Uses a simple heuristic:
    /// each message is at least 1 line, plus extra lines for long text
    /// that wraps at the current terminal width.
    pub fn rebuild_line_counts(&mut self, area_width: u16) {
        self.line_counts = self
            .messages
            .iter()
            .map(|msg| estimate_lines(msg, area_width))
            .collect();
    }

    /// Returns the total number of visual lines across all messages.
    pub fn total_lines(&self) -> usize {
        self.line_counts.iter().sum()
    }

    /// Returns the visual column of the cursor within the input line.
    pub fn cursor_column(&self) -> u16 {
        // Count chars before cursor (not bytes)
        let prefix_len = 2; // "> " prompt
        let col = if self.input_cursor == 0 {
            0
        } else {
            self.input[..self.input_cursor].chars().count()
        };
        (prefix_len + col) as u16
    }
}

/// Heuristic: count `\n` + estimate wrapped lines.
fn estimate_lines(msg: &ChatMessage, width: u16) -> usize {
    let w = width.max(1) as usize;

    let raw = match msg {
        ChatMessage::User { content } => {
            format!("> {content}")
        }
        ChatMessage::Assistant { content } => content.clone(),
        ChatMessage::Reasoning { content } => content.clone(),
        ChatMessage::ToolCall { name, args, state, .. } => match state {
            ToolCallState::Running => format!("  🔧 {name} {args}"),
            ToolCallState::Complete(output) => {
                format!("  ✓ {name} → {output}")
            }
        },
        ChatMessage::System { content } => format!("  ℹ {content}"),
        ChatMessage::Error { content } => content.clone(),
    };

    let mut lines = 0usize;
    for line in raw.lines() {
        if line.is_empty() {
            lines += 1;
        } else {
            lines += line.len().max(1).div_ceil(w);
        }
    }
    lines.max(1)
}

// ── Tests ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_lines_empty() {
        let msg = ChatMessage::Assistant {
            content: String::new(),
        };
        assert_eq!(estimate_lines(&msg, 80), 1); // at least 1
    }

    #[test]
    fn test_estimate_lines_short() {
        let msg = ChatMessage::User {
            content: "hi".into(),
        };
        assert_eq!(estimate_lines(&msg, 80), 1);
    }

    #[test]
    fn test_estimate_lines_newlines() {
        let msg = ChatMessage::Assistant {
            content: "line1\nline2\nline3".into(),
        };
        assert_eq!(estimate_lines(&msg, 80), 3);
    }

    #[test]
    fn test_estimate_lines_wrapping() {
        let msg = ChatMessage::Assistant {
            content: "a".repeat(200),
        };
        // 200 chars at width 80 → ceil(200/80) = 3
        assert_eq!(estimate_lines(&msg, 80), 3);
    }

    #[test]
    fn test_cursor_column_no_prompt() {
        // The cursor_column method includes the "> " prefix
        // This is tested via the App struct
    }
}
