//! # Rendering
//!
//! All ratatui drawing for the three-panel layout: chat area, input area,
//! and status bar.

use std::sync::atomic::Ordering;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use engine::CallOrigin;
use serde_json;

use super::app::App;
use super::keyboard::{cancel_shortcut_label, copy_shortcut_label};
use super::markdown::render_markdown;
use super::messages::{
    ChatMessage, SLASH_COMMANDS, SlashCompletionState, ThreadPicker, ToolCallState,
};
use super::theme;
use super::welcome;

// ── Layout ───────────────────────────────────────────────────────────────────────

/// Minimum visible text rows in the input area (before borders).
const MIN_INPUT_ROWS: u16 = 3;
/// Maximum visible text rows — beyond this the input area scrolls.
const MAX_INPUT_ROWS: u16 = 15;

/// Entry point called from the event loop on every frame.
///
/// Splits the terminal into three vertical regions and delegates to
/// [`draw_chat`], [`draw_input`], and [`draw_status`].
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Rebuild line counts for accurate scrolling computation
    app.rebuild_line_counts(area.width);

    // The input area grows with its content (wrapped rows), capped at
    // MAX_INPUT_ROWS — beyond that it scrolls (see draw_input). Input text
    // is wrapped manually (like the chat area) so cursor placement is exact;
    // `input_width` is the text width inside the input's border block.
    let input_width = area.width.saturating_sub(2).max(1);
    let metrics = input_metrics(&app.input, app.input_cursor, input_width);
    // Cap against the terminal height too (rows + 2 border + 1 status bar)
    // so the status bar never gets pushed off screen on short terminals.
    let max_rows = MAX_INPUT_ROWS.min(area.height.saturating_sub(4).max(MIN_INPUT_ROWS));
    let input_rows = (metrics.rows as u16).clamp(MIN_INPUT_ROWS, max_rows).max(1);

    let layout = Layout::vertical([
        Constraint::Fill(1),                // chat
        Constraint::Length(input_rows + 2), // input (rows + border)
        Constraint::Length(1),              // status bar
    ])
    .split(area);

    // Cache chat area for mouse coordinate mapping.
    app.chat_area = layout[0];

    // Keep the cursor row within the visible input window (follows the
    // cursor like the chat scroll offset).
    app.update_input_scroll_offset(metrics.cursor_row as u16, input_rows);

    draw_chat(frame, layout[0], app);
    draw_input(frame, layout[1], app);
    draw_status(frame, layout[2], app);

    // ── Slash Completion Popup ─────────────────────────────────
    if let Some(ref sc) = app.slash_completion {
        draw_slash_completion(frame, layout[1], sc);
    }

    // ── Thread Picker Overlay ──────────────────────────────────
    if let Some(ref picker) = app.thread_picker {
        draw_thread_picker(frame, area, picker);
    }

    // ── Help Overlay ───────────────────────────────────────────
    if app.show_help {
        draw_help_overlay(frame, area);
    }

    // Place the hardware cursor inside the input area. `metrics.cursor_col`
    // already includes the 2-col prompt prefix on row 0; add only the 1-col
    // left border. Subtract the scroll offset so the cursor follows the
    // visible window.
    frame.set_cursor_position((
        layout[1].x + 1 + metrics.cursor_col as u16,
        layout[1].y
            + 1
            + (metrics.cursor_row as u16).saturating_sub(app.input_scroll_offset as u16),
    ));
}

// ── Chat Area ────────────────────────────────────────────────────────────────────

/// Renders the scrollable conversation history with a right-edge scrollbar.
///
/// Lines are built once at a conservative width (reserving 1 column for the
/// scrollbar). When no scrollbar is needed the paragraph area expands to the
/// full width — the reserved column simply stays blank. This avoids a
/// dual-pass over all messages and keeps scroll math consistent.
///
/// The entire `area` is cleared before rendering so that scrollbar
/// appear/disappear transitions never leave residual characters at the
/// right edge.
fn draw_chat(frame: &mut Frame, area: Rect, app: &mut App) {
    // Show the current thread name in the border title so the user always
    // knows which conversation they're in (Nielsen #1: system status).
    let thread = app
        .conversation_title
        .as_deref()
        .map(|t| truncate_to_width(t, 40))
        .unwrap_or_else(|| "new".to_string());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Chat — {thread} "))
        .border_style(Style::default().fg(theme::BORDER));

    let inner = block.inner(area);
    let visible_height = inner.height.max(1) as usize;

    // Always reserve 1 column for the scrollbar so we only build lines
    // once. When no scrollbar is needed the extra column stays blank —
    // a negligible cost that eliminates dual-pass markdown rendering.
    let text_width = inner.width.saturating_sub(1).max(1);

    let raw_lines: Vec<Line<'_>> = app
        .messages
        .iter()
        .flat_map(|msg| {
            message_to_lines(msg, text_width, app.intervene_selection, app.spinner_frame)
        })
        .collect();
    let all_lines = wrap_to_width(raw_lines, text_width);
    let total_lines = all_lines.len();
    let has_scrollbar = total_lines > visible_height;
    // Cache for mouse-to-line coordinate mapping.
    app.total_rendered_lines = total_lines;
    app.visible_chat_height = visible_height;
    // Cache each wrapped line's plain text so selection copy can slice
    // exactly the highlighted display columns (see App::get_selection_text).
    app.rendered_chat_lines = all_lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect();

    // When scrollbar is visible, shrink the paragraph's rendering area by
    // 1 column so text and scrollbar don't overlap.
    let para_area = if has_scrollbar {
        Rect {
            width: area.width.saturating_sub(1).max(3),
            ..area
        }
    } else {
        area
    };

    // Compute scroll offset (offset = 0 means "show the bottom").
    let max_scroll = total_lines.saturating_sub(visible_height);
    let scroll = (max_scroll.saturating_sub(app.scroll_offset)).min(max_scroll) as u16;

    // Clear the FULL area so that residual characters from scrollbar
    // transitions cannot survive. The paragraph + scrollbar will
    // re-draw everything that should be visible.
    frame.render_widget(Clear, area);

    let paragraph = Paragraph::new(Text::from(all_lines))
        .block(block)
        .scroll((scroll, 0));

    frame.render_widget(paragraph, para_area);

    // ── Scrollbar — drawn in the rightmost column of `area` ──────
    if has_scrollbar {
        let scrollbar_x = area.x + area.width.saturating_sub(1);
        let scrollbar_area = Rect {
            x: scrollbar_x,
            y: inner.y,
            width: 1,
            height: inner.height,
        };

        // Scrollbar column was already cleared by the area-level Clear
        // above; clearing again is a cheap no-op for defense-in-depth.
        frame.render_widget(Clear, scrollbar_area);

        let thumb_pos = if total_lines == 0 {
            0.0
        } else {
            (scroll as f64) / (total_lines as f64)
        };
        let thumb_size = (visible_height as f64 / total_lines as f64).clamp(0.1, 1.0);

        let thumb_top = (thumb_pos * (visible_height as f64 - 1.0).max(0.0)).round() as u16;
        let thumb_height = ((thumb_size * visible_height as f64).round() as u16).max(1);

        for row in 0..inner.height {
            let y = scrollbar_area.y + row;
            if row >= thumb_top && row < thumb_top + thumb_height {
                frame.buffer_mut().set_string(
                    scrollbar_area.x,
                    y,
                    theme::SCROLL_THUMB,
                    Style::default().fg(theme::SCROLLBAR_THUMB),
                );
            } else {
                frame.buffer_mut().set_string(
                    scrollbar_area.x,
                    y,
                    theme::SCROLL_TRACK,
                    Style::default()
                        .fg(theme::SCROLLBAR_TRACK)
                        .add_modifier(Modifier::DIM),
                );
            }
        }
    }

    // ── Selection highlight overlay ────────────────────────────
    if let Some(ref sel) = app.selection {
        let (start_line, start_col, end_line, end_col) = sel.ordered_bounds();
        let highlight_bg = theme::SELECTION_BG;
        // Middle lines of a multi-line selection are highlighted to the
        // full text width; only the first/last lines are column-clipped.
        let full_width = inner.width as usize;

        for visible_row in 0..visible_height {
            let actual_line = scroll as usize + visible_row;
            if actual_line < start_line || actual_line > end_line {
                continue;
            }
            let (col_start, col_end) = if actual_line == start_line && actual_line == end_line {
                (start_col, end_col)
            } else if actual_line == start_line {
                (start_col, full_width)
            } else if actual_line == end_line {
                (0, end_col)
            } else {
                (0, full_width)
            };

            let y = inner.y + visible_row as u16;
            for column_offset in col_start..col_end.min(full_width) {
                let x = inner.x + column_offset as u16;
                if let Some(cell) = frame.buffer_mut().cell_mut((x, y))
                    && cell.symbol() != " "
                {
                    cell.bg = highlight_bg;
                }
            }
        }
    }
}

/// Converts one [`ChatMessage`] into styled ratatui [`Line`]s.
///
/// Each message gets a dim timestamp prefix on its first line. Tool calls
/// show the tool name prominently with args/output on a separate dim line.
fn message_to_lines(
    msg: &ChatMessage,
    area_width: u16,
    intervene_selection: Option<usize>,
    spinner_frame: usize,
) -> Vec<Line<'_>> {
    // ── Timestamp style (shared across all variants) ───────────────
    let ts_style = theme::ts_style();

    match msg {
        ChatMessage::User { content, timestamp } => {
            let mut lines = Vec::new();
            let content_lines: Vec<&str> = content.lines().collect();
            for (i, line) in content_lines.iter().enumerate() {
                if i == 0 {
                    lines.push(Line::from(vec![
                        Span::styled(format!("{timestamp} "), ts_style),
                        Span::styled(
                            theme::ICON_USER,
                            Style::default()
                                .fg(theme::ACCENT)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(*line, Style::default().fg(theme::TEXT_PRIMARY)),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::raw("       "), // align with timestamp + "> "
                        Span::styled(*line, Style::default().fg(theme::TEXT_PRIMARY)),
                    ]));
                }
            }
            lines
        }

        ChatMessage::Assistant { content, timestamp } => {
            let mut lines = render_markdown(content, area_width);
            // Prepend timestamp to the first rendered line
            if let Some(first) = lines.first_mut() {
                let mut spans = vec![Span::styled(format!("{timestamp} "), ts_style)];
                std::mem::swap(&mut first.spans, &mut spans);
                spans.extend(std::mem::take(&mut first.spans));
                first.spans = spans;
            }
            lines
        }

        ChatMessage::Reasoning { content, timestamp } => {
            let reasoning_style = Style::default()
                .fg(theme::WARNING)
                .add_modifier(Modifier::DIM)
                .add_modifier(Modifier::ITALIC);
            let content_lines: Vec<&str> = content.lines().collect();
            content_lines
                .iter()
                .enumerate()
                .map(|(i, line)| {
                    if i == 0 {
                        Line::from(vec![
                            Span::styled(format!("{timestamp} "), ts_style),
                            Span::styled(*line, reasoning_style),
                        ])
                    } else {
                        Line::from(Span::styled(*line, reasoning_style))
                    }
                })
                .collect()
        }

        ChatMessage::ToolCall {
            name,
            args,
            state,
            origin,
            progress_lines,
            timestamp,
            ..
        } => {
            let mut lines = Vec::new();

            // User-origin commands render like the old ShellOutput with "$" prefix.
            // LLM-origin tool calls render with ◌ / ✓ icons.
            let is_user = matches!(origin, CallOrigin::User);

            match state {
                ToolCallState::Running => {
                    if is_user {
                        // Header: "$ command" — green, like old ShellOutput.
                        lines.push(Line::from(vec![
                            Span::styled(format!("{timestamp} "), ts_style),
                            Span::styled(
                                theme::ICON_SHELL,
                                Style::default()
                                    .fg(theme::SUCCESS)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(args.as_str(), Style::default().fg(theme::SUCCESS)),
                        ]));
                        lines.push(Line::from(vec![
                            Span::raw("       "),
                            Span::styled(
                                "Running…",
                                Style::default()
                                    .fg(theme::WARNING)
                                    .add_modifier(Modifier::DIM),
                            ),
                        ]));
                    } else {
                        // Header: spinner + tool name + resource summary — yellow.
                        // The spinner icon animates: the event loop advances
                        // `spinner_frame` every SPINNER_INTERVAL_MS.
                        let spinner_icon =
                            theme::SPINNER_FRAMES[spinner_frame % theme::SPINNER_FRAMES.len()];
                        let resource = tool_resource_summary(name, args);
                        let mut header_spans = vec![
                            Span::styled(format!("{timestamp} "), ts_style),
                            Span::styled(
                                format!("{spinner_icon} "),
                                Style::default()
                                    .fg(theme::WARNING)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                name.as_str(),
                                Style::default()
                                    .fg(theme::WARNING)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ];
                        // Show the primary resource inline when available,
                        // e.g. "◌ read src/main.rs (running…)"
                        if let Some(ref res) = resource {
                            header_spans.push(Span::raw(" "));
                            header_spans.push(Span::styled(
                                res.to_owned(),
                                Style::default()
                                    .fg(theme::TEXT_PRIMARY)
                                    .add_modifier(Modifier::DIM),
                            ));
                        }
                        header_spans.push(Span::raw(" "));
                        header_spans.push(Span::styled("(running…)", ts_style));
                        lines.push(Line::from(header_spans));
                    }
                    // Accumulated progress lines (each ToolProgress appends one)
                    for msg in progress_lines {
                        let display = msg.lines().next().unwrap_or(msg);
                        let truncated =
                            truncate_to_width(display, area_width.saturating_sub(8) as usize);
                        lines.push(Line::from(vec![
                            Span::raw("       "),
                            Span::styled(
                                truncated,
                                Style::default()
                                    .fg(theme::WARNING)
                                    .add_modifier(Modifier::DIM),
                            ),
                        ]));
                    }
                }

                ToolCallState::Complete(output) => {
                    if is_user {
                        // Header: "$ command" — green.
                        lines.push(Line::from(vec![
                            Span::styled(format!("{timestamp} "), ts_style),
                            Span::styled(
                                theme::ICON_SHELL,
                                Style::default()
                                    .fg(theme::SUCCESS)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(args.as_str(), Style::default().fg(theme::SUCCESS)),
                        ]));
                        // Multi-line output — dim gray, like old ShellOutput.
                        for line in output.lines() {
                            lines.push(Line::from(vec![
                                Span::raw("       "),
                                Span::styled(
                                    line,
                                    Style::default()
                                        .fg(theme::TEXT_OUTPUT)
                                        .add_modifier(Modifier::DIM),
                                ),
                            ]));
                        }
                    } else {
                        // Header: checkmark + tool name — green.
                        lines.push(Line::from(vec![
                            Span::styled(format!("{timestamp} "), ts_style),
                            Span::styled(
                                theme::ICON_SUCCESS,
                                Style::default()
                                    .fg(theme::SUCCESS)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                name.as_str(),
                                Style::default()
                                    .fg(theme::SUCCESS)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]));
                        // Output preview line (if present)
                        if !output.is_empty() {
                            let preview = truncate_output(output, area_width);
                            if !preview.is_empty() {
                                lines.push(Line::from(vec![
                                    Span::raw("       "),
                                    Span::styled(
                                        preview,
                                        Style::default()
                                            .fg(theme::TEXT_OUTPUT)
                                            .add_modifier(Modifier::DIM),
                                    ),
                                ]));
                            }
                        }
                    }
                }

                ToolCallState::Rejected(reason) => {
                    // Header: ⊘ + tool name — yellow (policy decision, not an error).
                    lines.push(Line::from(vec![
                        Span::styled(format!("{timestamp} "), ts_style),
                        Span::styled(
                            theme::ICON_REJECTED,
                            Style::default()
                                .fg(theme::WARNING)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            name.as_str(),
                            Style::default()
                                .fg(theme::WARNING)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    // Rejection reason — yellow, dimmed.
                    let preview = truncate_output(reason, area_width);
                    if !preview.is_empty() {
                        lines.push(Line::from(vec![
                            Span::raw("       "),
                            Span::styled(
                                preview,
                                Style::default()
                                    .fg(theme::WARNING)
                                    .add_modifier(Modifier::DIM),
                            ),
                        ]));
                    }
                }

                ToolCallState::Error(error) => {
                    // Header: ✗ + tool name — red.
                    lines.push(Line::from(vec![
                        Span::styled(format!("{timestamp} "), ts_style),
                        Span::styled(
                            theme::ICON_ERROR,
                            Style::default()
                                .fg(theme::ERROR)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            name.as_str(),
                            Style::default()
                                .fg(theme::ERROR)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    // Error message — red, dimmed.
                    let preview = truncate_output(error, area_width);
                    if !preview.is_empty() {
                        lines.push(Line::from(vec![
                            Span::raw("       "),
                            Span::styled(
                                preview,
                                Style::default()
                                    .fg(theme::ERROR)
                                    .add_modifier(Modifier::DIM),
                            ),
                        ]));
                    }
                }
            }

            lines
        }

        ChatMessage::System { content, timestamp } => {
            let content_lines: Vec<&str> = content.lines().collect();
            content_lines
                .iter()
                .enumerate()
                .map(|(i, line)| {
                    let prefix = if i == 0 {
                        vec![
                            Span::styled(format!("{timestamp} "), ts_style),
                            Span::styled(
                                theme::ICON_INFO,
                                Style::default()
                                    .fg(theme::INFO)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]
                    } else {
                        vec![Span::raw("       ")]
                    };
                    let mut spans = prefix;
                    spans.push(Span::raw(*line));
                    Line::from(spans)
                })
                .collect()
        }

        ChatMessage::Welcome { model, workspace } => welcome::render(model, workspace, area_width),

        ChatMessage::Intervene {
            title,
            description,
            options,
            responded,
            chosen,
            custom_text,
            timestamp,
            ..
        } => {
            let mut lines = Vec::new();
            if *responded {
                let summary = if let Some(idx) = chosen {
                    let label = options.get(*idx).map(|s| s.as_str()).unwrap_or("?");
                    if let Some(text) = custom_text {
                        format!("{label}: {text}")
                    } else {
                        label.to_string()
                    }
                } else {
                    "Cancelled".to_string()
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{timestamp} "), ts_style),
                    Span::styled(
                        theme::ICON_SUCCESS,
                        Style::default()
                            .fg(theme::SUCCESS)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(title.as_str(), Style::default().fg(theme::WARNING)),
                    Span::raw(" "),
                    Span::styled(summary, Style::default().fg(theme::TEXT_PRIMARY)),
                ]));
            } else {
                // Title
                lines.push(Line::from(vec![
                    Span::styled(format!("{timestamp} "), ts_style),
                    Span::styled(
                        theme::ICON_INTERVENTION,
                        Style::default()
                            .fg(theme::WARNING)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        title.as_str(),
                        Style::default()
                            .fg(theme::WARNING)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                // Description — rendered as markdown so plan content
                // (headings, lists, code blocks, tables) displays
                // formatted instead of as raw markdown strings.
                let desc_width = area_width.saturating_sub(7); // account for indent
                let desc_lines = render_markdown(description, desc_width);
                for mut line in desc_lines {
                    // Prepend indent to the first span of each rendered line
                    let mut spans = vec![Span::raw("       ")];
                    std::mem::swap(&mut line.spans, &mut spans);
                    spans.extend(std::mem::take(&mut line.spans));
                    line.spans = spans;
                    lines.push(line);
                }
                // Options — each on its own line, highlighted when selected.
                if !options.is_empty() {
                    for (i, opt) in options.iter().enumerate() {
                        let is_selected = intervene_selection == Some(i);
                        let (prefix, style) = if is_selected {
                            (
                                "  ▶ ",
                                Style::default()
                                    .fg(theme::WARNING)
                                    .add_modifier(Modifier::BOLD),
                            )
                        } else {
                            (
                                "    ",
                                Style::default()
                                    .fg(theme::TEXT_SECONDARY)
                                    .add_modifier(Modifier::DIM),
                            )
                        };
                        lines.push(Line::from(vec![
                            Span::raw("       "),
                            Span::styled(format!("{prefix}{opt}"), style),
                        ]));
                    }
                }
            }
            lines
        }

        ChatMessage::Error { content, timestamp } => {
            let error_style = Style::default()
                .fg(theme::ERROR)
                .add_modifier(Modifier::BOLD);
            let content_lines: Vec<&str> = content.lines().collect();
            content_lines
                .iter()
                .enumerate()
                .map(|(i, line)| {
                    if i == 0 {
                        Line::from(vec![
                            Span::styled(format!("{timestamp} "), ts_style),
                            Span::styled(*line, error_style),
                        ])
                    } else {
                        Line::from(Span::styled(*line, error_style))
                    }
                })
                .collect()
        }
    }
}

/// Truncates tool output for compact inline display,
/// using terminal display width so CJK characters are counted correctly.
fn truncate_output(output: &str, width: u16) -> String {
    let max = (width as usize).saturating_sub(14).max(20);
    let one_line = output.replace('\n', " ");
    let display_width = UnicodeWidthStr::width(one_line.as_str());
    if display_width <= max {
        one_line
    } else {
        truncate_to_width(&one_line, max)
    }
}

/// Truncates `text` to fit within `max_width` display columns,
/// appending `…` when truncation occurs. Always cuts at a
/// valid UTF-8 character boundary.
fn truncate_to_width(text: &str, max_width: usize) -> String {
    let ellipsis_width = 1; // '…' is 1 column wide
    let limit = max_width.saturating_sub(ellipsis_width);
    let mut current_width = 0usize;
    let mut byte_end = 0usize;

    for (idx, ch) in text.char_indices() {
        let ch_width = UnicodeWidthStr::width(ch.encode_utf8(&mut [0u8; 4]));
        if current_width + ch_width > limit {
            break;
        }
        current_width += ch_width;
        byte_end = idx + ch.len_utf8();
    }

    if byte_end == 0 {
        "…".to_string()
    } else {
        format!("{}…", &text[..byte_end])
    }
}

/// Extracts a short primary-resource summary from tool JSON arguments for
/// inline display alongside the tool name during the Running state.
///
/// Returns `None` when args are empty, parse fails, or the tool name is
/// unrecognised — the caller falls back to the current format.
fn tool_resource_summary(name: &str, args_json: &str) -> Option<String> {
    if args_json.is_empty() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(args_json).ok()?;
    let obj = value.as_object()?;

    let raw = match name {
        // File-path tools
        "read" | "write" | "edit" => obj.get("file_path")?.as_str()?.to_string(),

        // Pattern tools
        "glob" | "grep" => obj.get("pattern")?.as_str()?.to_string(),

        // Shell
        "shell" => obj.get("command")?.as_str()?.to_string(),

        // LS: path is optional — show "root" when absent or null
        "ls" => match obj.get("path") {
            Some(v) if !v.is_null() => v.as_str()?.to_string(),
            _ => "root".to_string(),
        },

        // Question tools
        "ask_user_question" | "ask_user" => obj.get("question")?.as_str()?.to_string(),

        // Calculator
        "calculator" => obj.get("expression")?.as_str()?.to_string(),

        // Todo: show item count
        "todo" => {
            let count = obj.get("todos")?.as_array()?.len();
            return Some(format!("{} items", count));
        }

        // Echo
        "echo" => obj.get("text")?.as_str()?.to_string(),

        // Subagent / task
        "subagent" | "task" => obj.get("description")?.as_str()?.to_string(),

        // Unknown tool — no summary
        _ => return None,
    };

    // Truncate very long values to ~40 display columns with "…" suffix
    const MAX_SUMMARY_WIDTH: usize = 40;
    let display_width = UnicodeWidthStr::width(raw.as_str());
    if display_width > MAX_SUMMARY_WIDTH {
        Some(truncate_to_width(&raw, MAX_SUMMARY_WIDTH))
    } else {
        Some(raw)
    }
}

/// Wraps each [`Line`] to `max_width` display columns, splitting wide
/// lines so the returned `Vec` length accurately reflects visual rows.
///
/// Ratatui's `Paragraph` would wrap lines internally, but we can't count
/// those extra rows — so we wrap manually here for correct scroll math.
///
/// Wrapping is span-aware: each span keeps its own style, and we only
/// split individual wide spans at display-width boundaries. This
/// preserves markdown styling (bold, italic, code blocks, links, etc.)
/// across wrapped lines.
fn wrap_to_width(lines: Vec<Line<'_>>, max_width: u16) -> Vec<Line<'_>> {
    let max_w = max_width.max(1) as usize;
    let mut out = Vec::with_capacity(lines.len());

    for line in lines {
        let total_w: usize = line
            .spans
            .iter()
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum();

        if total_w <= max_w {
            out.push(line);
            continue;
        }

        // Line is too wide — wrap span-by-span, preserving each style.
        let mut current_spans: Vec<Span<'_>> = Vec::new();
        let mut current_w: usize = 0;

        for span in line.spans.into_iter() {
            let span_w = UnicodeWidthStr::width(span.content.as_ref());

            if current_w + span_w <= max_w {
                // Fits on the current wrapped line.
                current_spans.push(span);
                current_w += span_w;
            } else if current_w == 0 {
                // Single span is wider than max_w — split it across
                // multiple lines, each inheriting this span's style.
                let mut rem: &str = span.content.as_ref();
                while !rem.is_empty() {
                    let (chunk, rest) = split_at_display_width(rem, max_w);
                    if chunk.is_empty() {
                        break;
                    }
                    out.push(Line::from(Span::styled(chunk.to_string(), span.style)));
                    rem = rest;
                }
            } else {
                // Doesn't fit on current line — flush and start a new
                // wrapped line with this span.
                out.push(Line::from(std::mem::take(&mut current_spans)));
                current_spans.push(span);
                current_w = span_w;
            }
        }

        if !current_spans.is_empty() {
            out.push(Line::from(current_spans));
        }
    }

    out
}

/// Splits `text` at the closest valid boundary to `max_width` display columns.
///
/// Returns `(before, after)` where `before` fits within `max_width` columns
/// and `after` is the rest (possibly empty). Always splits at a UTF-8
/// character boundary.
fn split_at_display_width(text: &str, max_width: usize) -> (&str, &str) {
    if text.is_empty() {
        return ("", "");
    }

    let mut width = 0usize;
    let mut byte_pos = 0usize;

    for (idx, ch) in text.char_indices() {
        let ch_w = UnicodeWidthStr::width(ch.encode_utf8(&mut [0u8; 4]));
        if width + ch_w > max_width {
            break;
        }
        width += ch_w;
        byte_pos = idx + ch.len_utf8();
    }

    if byte_pos == 0 {
        // Even one character doesn't fit — force at least one char
        let first_char = text.chars().next().unwrap();
        (
            text.get(..first_char.len_utf8()).unwrap(),
            text.get(first_char.len_utf8()..).unwrap_or(""),
        )
    } else {
        (
            text.get(..byte_pos).unwrap(),
            text.get(byte_pos..).unwrap_or(""),
        )
    }
}

// ── Input Area ───────────────────────────────────────────────────────────────────

/// Renders the text input area with a border and cursor.
/// Supports multi-line input — displays all lines, with cursor
/// highlighting on the active line.
fn draw_input(frame: &mut Frame, area: Rect, app: &App) {
    let has_intervene = app.has_pending_intervene();
    let plan_active = app.plan_mode.active.load(Ordering::SeqCst);
    let spinner_icon = theme::SPINNER_FRAMES[app.spinner_frame % theme::SPINNER_FRAMES.len()];

    // Title/border reflect the current interaction mode (Nielsen #1):
    // intervention > shell confirm > plan mode > streaming > idle.
    let (style, title) = if has_intervene && app.intervene_text_mode {
        (
            Style::default().fg(theme::BORDER_CHOOSE),
            " Answer ".to_string(),
        )
    } else if has_intervene {
        (
            Style::default().fg(theme::BORDER_CHOOSE),
            " Choose ".to_string(),
        )
    } else if app.pending_shell_confirm.is_some() {
        (
            Style::default().fg(theme::BORDER_CONFIRM),
            " Confirm Shell ".to_string(),
        )
    } else if plan_active && app.streaming {
        (
            Style::default().fg(theme::BORDER_PLAN),
            format!(" [PLAN] {spinner_icon} Inject "),
        )
    } else if plan_active {
        (
            Style::default().fg(theme::BORDER_PLAN),
            " [PLAN] Input ".to_string(),
        )
    } else if app.streaming {
        (
            Style::default().fg(theme::BORDER_INJECT),
            format!(" {spinner_icon} Inject "),
        )
    } else {
        (
            Style::default().fg(theme::BORDER_INPUT),
            " Input ".to_string(),
        )
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(style);

    let cursor_style = if app.streaming {
        Style::default()
            .bg(theme::TEXT_SECONDARY)
            .fg(theme::TEXT_PRIMARY)
    } else {
        Style::default()
            .bg(theme::TEXT_PRIMARY)
            .fg(theme::CURSOR_FG)
    };

    let display_lines = build_input_lines(
        &app.input,
        app.input_cursor,
        cursor_style,
        area.width.saturating_sub(2).max(1),
    );

    // Show a hint when the input is empty
    let lines: Vec<Line<'_>> = if app.pending_shell_confirm.is_some() {
        vec![
            Line::from(Span::raw(" ")),
            Line::from(Span::styled(
                " y/Enter run  ·  n/Esc cancel",
                theme::hint_style(),
            )),
        ]
    } else if app.input.is_empty() && has_intervene && app.intervene_text_mode {
        vec![
            Line::from(Span::raw(" ")),
            Line::from(Span::styled(
                " Type your response and press Enter. Esc to go back.",
                theme::hint_style(),
            )),
        ]
    } else if app.input.is_empty() && has_intervene {
        vec![
            Line::from(Span::raw(" ")),
            Line::from(Span::styled(
                " ↑↓ to navigate  ·  Enter to select  ·  Esc to cancel",
                theme::hint_style(),
            )),
        ]
    } else if app.input.is_empty() && app.streaming {
        vec![
            Line::from(Span::raw(" ")),
            Line::from(Span::styled(
                " Type to inject a hint while the agent is running. Enter to send.",
                theme::hint_style(),
            )),
        ]
    } else if app.input.is_empty() && !app.streaming {
        vec![
            Line::from(Span::raw(" ")),
            Line::from(Span::styled(
                " Type a message and press Enter. Shift+Enter for newline. /help for commands.",
                theme::hint_style(),
            )),
        ]
    } else {
        display_lines
    };

    // Clear residual characters from previous frame before rendering.
    frame.render_widget(Clear, area);

    // Lines are pre-wrapped (build_input_lines) so each Line is exactly one
    // visual row; the scroll offset is a direct row offset. Manual wrapping
    // keeps the cursor placement in draw() exact.
    let paragraph = Paragraph::new(lines)
        .scroll((app.input_scroll_offset as u16, 0))
        .block(block);
    frame.render_widget(paragraph, area);
}

/// The `"> "` prompt prefix span, styled consistently on every input row.
fn prefix_span() -> Span<'static> {
    Span::styled(
        theme::ICON_USER,
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::BOLD),
    )
}

/// Builds the styled input lines, wrapping long lines to `width` columns —
/// the exact layout [`input_metrics`] measures.
///
/// Each `\n`-separated logical line becomes one or more visual rows: the
/// first row carries the `"> "` prompt prefix (leaving `width - 2` columns
/// of text), continuation rows start at the left edge and fill `width`.
/// The cursor character is highlighted on its row; a cursor at the end of
/// a row shows as a highlighted trailing space.
fn build_input_lines(input: &str, cursor: usize, cursor_style: Style, width: u16) -> Vec<Line<'_>> {
    let w = (width as usize).max(1);
    let first_cap = w.saturating_sub(2).max(1);
    let mut lines: Vec<Line<'_>> = Vec::new();

    let mut line_start = 0usize;
    // Only one chunk can carry the cursor (mirrors `input_metrics`); once
    // rendered, later chunks on the same logical line stay plain.
    let mut found = false;
    while line_start <= input.len() {
        let rel_end = input[line_start..]
            .find('\n')
            .unwrap_or(input.len() - line_start);
        let line = &input[line_start..line_start + rel_end];
        let cursor_in_line = cursor.saturating_sub(line_start);
        let cursor_here = cursor_in_line <= line.len();

        if line.is_empty() {
            // Single row: prefix, plus the cursor space when it's on this
            // (empty) line.
            let mut spans = vec![prefix_span()];
            if cursor_here {
                spans.push(Span::styled(" ", cursor_style));
            }
            lines.push(Line::from(spans));
        } else {
            let mut offset = 0usize;
            let mut cap = first_cap;
            let mut first_row = true;
            while offset < line.len() {
                let (chunk, _) = split_at_display_width(&line[offset..], cap);
                if chunk.is_empty() {
                    break;
                }
                let chunk_end = offset + chunk.len();
                // The cursor belongs to this chunk when it sits strictly
                // inside it (and not in an earlier chunk), or exactly at its
                // end when that end is the end of the logical line (a
                // mid-line boundary belongs to the next chunk, whose row
                // starts there).
                let cursor_in_chunk = cursor_here
                    && !found
                    && cursor_in_line >= offset
                    && (cursor_in_line < chunk_end || chunk_end == line.len());

                let mut spans: Vec<Span<'_>> = Vec::new();
                if first_row {
                    spans.push(prefix_span());
                }
                if cursor_in_chunk {
                    found = true;
                    spans.push(Span::raw(&line[offset..cursor_in_line]));
                    if cursor_in_line < chunk_end {
                        let ch = line[cursor_in_line..].chars().next().unwrap_or(' ');
                        spans.push(Span::styled(ch.to_string(), cursor_style));
                        let after_start = cursor_in_line + ch.len_utf8();
                        if after_start < chunk_end {
                            spans.push(Span::raw(&line[after_start..chunk_end]));
                        }
                    } else {
                        // Cursor at end of the logical line — trailing space.
                        // The hardware cursor sits on this cell. If the last
                        // character was wide (CJK, 2 terminal columns), also
                        // restyle the cell to its right with a visually-blank
                        // style: that cell carried the wide char's right half,
                        // and without a change there the buffer diff would
                        // skip it, leaving a ghost half-character on the
                        // terminal after the char is deleted. (A wide char is
                        // itself rewritten over both columns; only the narrow
                        // trailing space needs this.)
                        spans.push(Span::styled(" ", cursor_style));
                        let prev_w = line[..cursor_in_line]
                            .chars()
                            .next_back()
                            .map(|c| {
                                let mut b = [0u8; 4];
                                UnicodeWidthStr::width(c.encode_utf8(&mut b))
                            })
                            .unwrap_or(1);
                        if prev_w >= 2 {
                            spans.push(Span::styled(
                                " ",
                                Style::default().fg(theme::TEXT_SECONDARY),
                            ));
                        }
                    }
                } else {
                    spans.push(Span::raw(chunk));
                }
                lines.push(Line::from(spans));

                offset = chunk_end;
                cap = w;
                first_row = false;
            }
        }

        if line_start + rel_end >= input.len() {
            break;
        }
        line_start += rel_end + 1;
    }

    lines
}

// ── Status Bar ───────────────────────────────────────────────────────────────────

/// Renders the single-line status bar at the bottom with better styling.
fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let (left_spans, accent_text, right) = build_status_content(app);

    let left_width: usize = left_spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    let accent_width = UnicodeWidthStr::width(accent_text.as_str());
    let right_width = UnicodeWidthStr::width(right.as_str());
    let total_space = area.width as usize;

    // When space runs out, drop the right-side hint first — it is
    // nice-to-have, while the left side carries the actual state.
    let right = if left_width + accent_width + right_width + 1 > total_space {
        String::new()
    } else {
        right
    };
    let right_width = UnicodeWidthStr::width(right.as_str());

    let gap = total_space
        .saturating_sub(left_width + accent_width + right_width)
        .max(1);

    let mut spans = left_spans;
    spans.push(Span::styled(
        " ".repeat(gap),
        Style::default().fg(theme::STATUS_FG).bg(theme::STATUS_BG),
    ));
    let accent_color = if app.streaming {
        theme::STATUS_ACCENT_STREAMING
    } else {
        theme::STATUS_ACCENT_IDLE
    };
    spans.push(Span::styled(
        accent_text,
        Style::default().fg(accent_color).bg(theme::STATUS_BG),
    ));
    spans.push(Span::styled(
        right,
        Style::default()
            .fg(theme::TEXT_SECONDARY)
            .bg(theme::STATUS_BG),
    ));

    // Clear residual characters from previous frame before rendering.
    frame.render_widget(Clear, area);

    let paragraph = Paragraph::new(Line::from(spans));
    frame.render_widget(paragraph, area);
}

// ── Thread Picker Overlay ─────────────────────────────────────────────────────────

/// Draws a centered popup overlay for selecting a saved conversation thread.
///
/// The overlay covers ~70% width and is vertically centered. The selected
/// thread is highlighted in cyan; others are dimmed.
fn draw_thread_picker(frame: &mut Frame, area: Rect, picker: &ThreadPicker) {
    let threads = &picker.threads;
    let selected = picker.selected;

    let popup_width = (area.width as f32 * 0.7) as u16;
    let popup_height = (threads.len() + 4).min(14) as u16;

    let popup_x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let popup_y = area.y + (area.height.saturating_sub(popup_height)) / 2;

    let popup_rect = Rect {
        x: popup_x,
        y: popup_y,
        width: popup_width,
        height: popup_height,
    };

    // Clear the area behind the popup
    frame.render_widget(Clear, popup_rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Resume Conversation ")
        .border_style(Style::default().fg(theme::ACCENT))
        .style(Style::default().bg(theme::OVERLAY_BG));

    let inner = block.inner(popup_rect);

    // Build lines for each thread
    let mut lines: Vec<Line<'_>> = Vec::new();

    for (i, t) in threads.iter().enumerate() {
        let is_selected = i == selected;

        let marker = if is_selected { " ▶ " } else { "   " };
        let info = format!(
            "{name:20}  {count:4} msgs  {chars:6} chars  {time}",
            name = t.name,
            count = t.message_count,
            chars = format_human(t.total_chars, "k", "M"),
            time = t.saved_at,
        );

        let style = if is_selected {
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::TEXT_SECONDARY)
        };

        lines.push(Line::from(vec![
            Span::styled(marker, style),
            Span::styled(info, style),
        ]));
    }

    // Add a blank spacer line before the footer
    while lines.len() < inner.height.saturating_sub(1) as usize {
        lines.push(Line::from(""));
    }

    // Footer
    let footer = Line::from(Span::styled(
        " ↑↓ navigate   Enter select   Esc cancel ",
        Style::default()
            .fg(theme::TEXT_SECONDARY)
            .add_modifier(Modifier::DIM),
    ));
    lines.push(footer);

    let paragraph = Paragraph::new(Text::from(lines)).block(block);
    frame.render_widget(paragraph, popup_rect);
}

// ── Slash Completion Popup ──────────────────────────────────────────────────────

/// Draws the slash-command completion popup directly above the input area
/// (Nielsen #6: recognition rather than recall). Covers the bottom rows of
/// the chat area — standard popup behavior, same as the thread picker.
fn draw_slash_completion(frame: &mut Frame, input_area: Rect, sc: &SlashCompletionState) {
    let max_visible = sc.matches.len().min(8);
    let popup_height = (max_visible + 2) as u16; // + top/bottom border
    let popup_rect = Rect {
        x: input_area.x,
        y: input_area.y.saturating_sub(popup_height),
        width: input_area.width.min(60),
        height: popup_height,
    };

    frame.render_widget(Clear, popup_rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Commands — Tab accept · Enter run · Esc dismiss ")
        .border_style(Style::default().fg(theme::ACCENT))
        .style(Style::default().bg(theme::OVERLAY_BG));

    // Slide the visible window so the selected item is always in view.
    let start = if sc.selected < max_visible {
        0
    } else {
        (sc.selected + 1)
            .saturating_sub(max_visible)
            .min(sc.matches.len().saturating_sub(max_visible))
    };

    let lines: Vec<Line<'_>> = sc
        .matches
        .iter()
        .skip(start)
        .take(max_visible)
        .enumerate()
        .map(|(i, cmd)| {
            let is_selected = i + start == sc.selected;
            let (marker, style) = if is_selected {
                (
                    theme::ICON_SELECTED,
                    Style::default()
                        .fg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("  ", Style::default().fg(theme::TEXT_SECONDARY))
            };
            Line::from(vec![
                Span::styled(marker, style),
                Span::styled(format!("{:<14}", cmd.usage), style),
                Span::styled(
                    cmd.desc,
                    Style::default()
                        .fg(theme::TEXT_DIM)
                        .add_modifier(Modifier::DIM),
                ),
            ])
        })
        .collect();

    let paragraph = Paragraph::new(Text::from(lines)).block(block);
    frame.render_widget(paragraph, popup_rect);
}

// ── Help Overlay ────────────────────────────────────────────────────────────────

/// Draws a centered help overlay with all commands, key bindings, and shell
/// prefixes (Nielsen #10: help and documentation). Dismissed by any key.
fn draw_help_overlay(frame: &mut Frame, area: Rect) {
    let popup_width = ((area.width as f32 * 0.75) as u16).min(76);
    let popup_height = area.height.saturating_sub(4).min(32);
    let popup_rect = Rect {
        x: area.x + area.width.saturating_sub(popup_width) / 2,
        y: area.y + area.height.saturating_sub(popup_height) / 2,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help — press any key to close ")
        .border_style(Style::default().fg(theme::ACCENT))
        .style(Style::default().bg(theme::OVERLAY_BG));

    let section = |title: &str| {
        Line::from(Span::styled(
            title.to_string(),
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ))
    };
    let text = Style::default().fg(theme::TEXT_DIM);
    let dim = theme::hint_style();

    let mut lines: Vec<Line<'static>> = vec![section("Commands")];
    for cmd in SLASH_COMMANDS {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<16}", cmd.usage), text),
            Span::styled(cmd.desc, dim),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(section("Keys"));
    for (k, d) in [
        ("Enter", "send · inject hint while running"),
        ("Shift+Enter", "insert newline"),
        ("Tab / Right", "accept slash completion"),
        ("Up / Down", "input history · completion nav"),
        ("PgUp / PgDn", "scroll chat · mouse wheel works too"),
        ("Esc", "cancel generation · close popup · clear selection"),
        // The copy key is OS-native (Cmd+C on macOS, Ctrl+C elsewhere);
        // on non-macOS one key both copies and cancels.
        (
            copy_shortcut_label(),
            if cfg!(target_os = "macos") {
                "copy selected text"
            } else {
                "copy selection · cancel generation"
            },
        ),
        ("Ctrl+D", "exit (empty input) · delete char forward"),
        ("Ctrl+R", "retry the last submission after a failure"),
        ("? / F1", "toggle this help"),
    ] {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<16}", k), text),
            Span::styled(d, dim),
        ]));
    }

    // On macOS the copy key (Cmd+C) and the cancel key (Ctrl+C) are
    // distinct — the entry above covers copy, so cancel gets its own line.
    if cfg!(target_os = "macos") {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<16}", cancel_shortcut_label()), text),
            Span::styled("cancel generation", dim),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(section("Shell"));
    for (k, d) in [
        ("!<cmd>", "run a shell command, share output with the agent"),
        ("!!text", "send a literal message starting with !"),
    ] {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<16}", k), text),
            Span::styled(d, dim),
        ]));
    }

    let paragraph = Paragraph::new(Text::from(lines)).block(block);
    frame.render_widget(paragraph, popup_rect);
}

/// Formats a number with a human-readable suffix (e.g. "2.5k", "1.2K", "3.0M").
fn format_human(n: usize, suffix_lower: &str, suffix_upper: &str) -> String {
    if n >= 1_000_000 {
        format!("{:.1}{suffix_upper}", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}{suffix_lower}", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Builds the left spans, accent text, and right hint of the status bar.
fn build_status_content(app: &App) -> (Vec<Span<'static>>, String, String) {
    let base = Style::default().fg(theme::STATUS_FG).bg(theme::STATUS_BG);
    let msgs = app.messages.len();

    let mut left_spans: Vec<Span<'static>> = vec![Span::styled(" ", base)];

    // Plan-mode badge — bold amber so the restrictive mode is hard to miss
    // (Nielsen #1: visibility of system status).
    if app.plan_mode.active.load(Ordering::SeqCst) {
        left_spans.push(Span::styled(
            " PLAN ",
            Style::default()
                .fg(theme::WARNING)
                .bg(theme::STATUS_BG)
                .add_modifier(Modifier::BOLD),
        ));
        left_spans.push(Span::styled("| ", base));
    }

    // Build todo progress snippet
    let todo_part = {
        let todos = app.todos.read().ok();
        todos
            .filter(|t| !t.is_empty())
            .map(|todos| {
                let total = todos.len();
                let done = todos.iter().filter(|t| t.status == "completed").count();
                let in_progress = todos
                    .iter()
                    .find(|t| t.status == "in_progress")
                    .map(|t| t.active_form.as_str());
                match in_progress {
                    Some(active) => format!("☐ {done}/{total} · ✍ {active} | "),
                    None => format!("☐ {done}/{total} | "),
                }
            })
            .unwrap_or_default()
    };

    // Build trace metrics snippet (only shown when a run has started).
    let trace_part = {
        let m = &app.trace_store.metrics;
        if m.run_started.load(std::sync::atomic::Ordering::Relaxed) {
            let steps = m.steps();
            let llm = m.llm_calls();
            let tools = m.tool_calls();
            let tokens = format_human(m.total_tokens() as usize, "K", "M");
            format!("#{steps} · {llm} LLM · {tools} tools · {tokens} tok | ")
        } else {
            String::new()
        }
    };

    let model = truncate_to_width(&app.model, 20);
    left_spans.push(Span::styled(
        format!("{todo_part}{trace_part}{model} | {msgs} msgs "),
        base,
    ));

    // Accent: animated spinner + elapsed seconds while the agent runs —
    // the primary "system is working" signal (Nielsen #1).
    let accent = if app.streaming {
        let frame = theme::SPINNER_FRAMES[app.spinner_frame % theme::SPINNER_FRAMES.len()];
        let elapsed = app
            .run_started_at
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        format!(" {frame} thinking… {elapsed}s ")
    } else {
        String::new()
    };

    // Right: context-sensitive key hints (Nielsen #6: recognition rather
    // than recall) — the bindings that matter in the current state.
    let right = if app.has_pending_intervene() {
        " ↑↓ nav · Enter select · Esc cancel  ".to_string()
    } else if app.streaming {
        format!(
            "Esc cancel · Enter inject{}  ",
            if app.scroll_offset > 0 {
                format!(" · ↑{}", app.scroll_offset)
            } else {
                String::new()
            }
        )
    } else if app.scroll_offset > 0 {
        format!("↑{} scrolled · PgDn bottom  ", app.scroll_offset)
    } else {
        " Enter send · ↑ hist · / cmds · ? help  ".to_string()
    };

    (left_spans, accent, right)
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

    /// Keeps the input-area scroll offset so the cursor row stays inside
    /// the visible window: scrolls up when the cursor passes the top,
    /// scrolls down when it passes the bottom. No-op while the input fits.
    pub fn update_input_scroll_offset(&mut self, cursor_visual_row: u16, visible_rows: u16) {
        let cursor = cursor_visual_row as usize;
        let visible = visible_rows as usize;
        if cursor < self.input_scroll_offset {
            self.input_scroll_offset = cursor;
        } else if cursor >= self.input_scroll_offset + visible {
            self.input_scroll_offset = cursor.saturating_sub(visible.saturating_sub(1));
        }
    }
}

/// Layout metrics for the wrapped input area, computed by [`input_metrics`].
struct InputMetrics {
    /// Total visual rows the input occupies (always ≥ 1).
    rows: usize,
    /// 0-based visual row of the cursor.
    cursor_row: usize,
    /// Display column of the cursor within its row — includes the 2-col
    /// prompt prefix when the row is the first row of a logical line.
    cursor_col: usize,
}

/// Walks the input text, splitting each `\n`-separated logical line into
/// display-width chunks (the first chunk of each line leaves 2 columns for
/// the `"> "` prompt prefix, subsequent chunks fill the full width — the
/// exact layout [`build_input_lines`] renders). Returns the total visual
/// row count plus the cursor's (row, column), CJK-aware via
/// [`split_at_display_width`].
fn input_metrics(input: &str, cursor: usize, width: u16) -> InputMetrics {
    let w = (width as usize).max(1);
    let first_cap = w.saturating_sub(2).max(1);
    let mut rows = 0usize;
    let mut cursor_row = 0usize;
    let mut cursor_col = 0usize;
    let mut found = false;

    let mut line_start = 0usize;
    while line_start <= input.len() {
        let rel_end = input[line_start..]
            .find('\n')
            .unwrap_or(input.len() - line_start);
        let line = &input[line_start..line_start + rel_end];
        let cursor_in_line = cursor.saturating_sub(line_start);
        let cursor_here = cursor_in_line <= line.len();
        let line_w = UnicodeWidthStr::width(line);

        if line_w <= first_cap {
            // Single row: prefix + whole line.
            if cursor_here && !found {
                found = true;
                cursor_row = rows;
                cursor_col = 2 + UnicodeWidthStr::width(&line[..cursor_in_line]);
            }
            rows += 1;
        } else {
            // Wrapped across multiple rows: first chunk at first_cap, the
            // rest at `w`.
            let mut offset = 0usize;
            let mut cap = first_cap;
            while offset < line.len() {
                let (chunk, _) = split_at_display_width(&line[offset..], cap);
                if chunk.is_empty() {
                    break;
                }
                let chunk_end = offset + chunk.len();
                // Same rule as build_input_lines: the cursor belongs to this
                // chunk when strictly inside it (and not in an earlier
                // chunk), or at its end when that end is the end of the
                // logical line.
                if cursor_here
                    && !found
                    && cursor_in_line >= offset
                    && (cursor_in_line < chunk_end || chunk_end == line.len())
                {
                    found = true;
                    cursor_row = rows;
                    let prefix = if offset == 0 { 2 } else { 0 };
                    cursor_col = prefix + UnicodeWidthStr::width(&line[offset..cursor_in_line]);
                }
                rows += 1;
                offset = chunk_end;
                cap = w;
            }
        }

        if line_start + rel_end >= input.len() {
            break;
        }
        line_start += rel_end + 1;
    }

    InputMetrics {
        rows: rows.max(1),
        cursor_row,
        cursor_col,
    }
}

/// Heuristic: count `\n` + estimate wrapped lines.
/// Uses terminal display width so CJK characters are counted as 2 columns.
fn estimate_lines(msg: &ChatMessage, width: u16) -> usize {
    let w = width.max(1) as usize;

    let raw = match msg {
        ChatMessage::User { content, .. } => {
            format!("> {content}")
        }
        ChatMessage::Assistant { content, .. } => content.clone(),
        ChatMessage::Reasoning { content, .. } => content.clone(),
        ChatMessage::ToolCall {
            name,
            args,
            state,
            origin,
            ..
        } => {
            let is_user = matches!(origin, CallOrigin::User);
            match state {
                ToolCallState::Running => {
                    if is_user {
                        format!("$ {args}\nRunning…")
                    } else {
                        format!("  🔧 {name} {args}")
                    }
                }
                ToolCallState::Complete(output) => {
                    if is_user {
                        format!("$ {args}\n{output}")
                    } else {
                        format!("  ✓ {name} → {output}")
                    }
                }
                ToolCallState::Rejected(reason) => {
                    format!("  ⊘ {name} → {reason}")
                }
                ToolCallState::Error(error) => {
                    format!("  ✗ {name} → {error}")
                }
            }
        }
        ChatMessage::System { content, .. } => format!("  ℹ {content}"),
        // The banner is ASCII art — its visual line count is exact, no wrapping.
        // `estimate_lines` receives the full chat width, while `render` sees
        // the inner text width (borders + scrollbar) — align the threshold
        // decision by subtracting the same 3 columns.
        ChatMessage::Welcome { .. } => return welcome::line_count(width.saturating_sub(3)),
        ChatMessage::Intervene {
            title,
            description,
            options,
            responded,
            chosen,
            custom_text,
            ..
        } => {
            if *responded {
                if let Some(idx) = chosen {
                    let label = options.get(*idx).map(|s| s.as_str()).unwrap_or("?");
                    if let Some(text) = custom_text {
                        format!("  ✓ {title} → {label}: {text}")
                    } else {
                        format!("  ✓ {title} → {label}")
                    }
                } else {
                    format!("  ✓ {title} → Cancelled")
                }
            } else {
                let opts = options.join(" / ");
                format!("  ⚡ {title}\n{description}\n  [{opts}]")
            }
        }
        ChatMessage::Error { content, .. } => content.clone(),
    };

    let mut lines = 0usize;
    for line in raw.lines() {
        if line.is_empty() {
            lines += 1;
        } else {
            // Use Unicode display width, not byte length
            let display_width = UnicodeWidthStr::width(line).max(1);
            lines += display_width.div_ceil(w);
        }
    }
    lines.max(1)
}

// ── Tests ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ts() -> String {
        "00:00:00".into()
    }

    #[test]
    fn test_estimate_lines_empty() {
        let msg = ChatMessage::Assistant {
            content: String::new(),
            timestamp: ts(),
        };
        assert_eq!(estimate_lines(&msg, 80), 1); // at least 1
    }

    #[test]
    fn test_estimate_lines_short() {
        let msg = ChatMessage::User {
            content: "hi".into(),
            timestamp: ts(),
        };
        assert_eq!(estimate_lines(&msg, 80), 1);
    }

    #[test]
    fn test_estimate_lines_newlines() {
        let msg = ChatMessage::Assistant {
            content: "line1\nline2\nline3".into(),
            timestamp: ts(),
        };
        assert_eq!(estimate_lines(&msg, 80), 3);
    }

    #[test]
    fn test_estimate_lines_wrapping() {
        let msg = ChatMessage::Assistant {
            content: "a".repeat(200),
            timestamp: ts(),
        };
        // 200 chars at width 80 → ceil(200/80) = 3
        assert_eq!(estimate_lines(&msg, 80), 3);
    }

    #[test]
    fn test_cjk_estimate_lines() {
        // Chinese characters are 2 columns wide but 3 bytes each in UTF-8.
        // 10 Chinese chars = 30 bytes but 20 columns wide.
        let msg = ChatMessage::Assistant {
            content: "你好世界你好世界你好世界你好世界你好世界".into(), // 20 chars, 60 bytes, 40 cols
            timestamp: ts(),
        };
        // 40 columns at width 80 → 1 line (was 60/80=1 with byte length, same in this case)
        assert_eq!(estimate_lines(&msg, 80), 1);

        // Now test narrow width: 40 cols at width 30 → ceil(40/30) = 2
        assert_eq!(estimate_lines(&msg, 30), 2);
    }

    // ── input_metrics / build_input_lines tests ─────────────────

    #[test]
    fn test_input_metrics_empty() {
        let m = input_metrics("", 0, 80);
        assert_eq!(m.rows, 1);
        assert_eq!(m.cursor_row, 0);
        assert_eq!(m.cursor_col, 2); // after the "> " prefix
    }

    #[test]
    fn test_input_metrics_single_line() {
        let m = input_metrics("hello", 5, 80);
        assert_eq!(m.rows, 1);
        assert_eq!(m.cursor_row, 0);
        assert_eq!(m.cursor_col, 7); // 2 prefix + 5 text
    }

    #[test]
    fn test_input_metrics_wrapped() {
        // width 10 → first chunk 8 (prefix leaves 8), continuation 10.
        let text = "0123456789abc"; // 13 wide → 2 rows
        let m = input_metrics(text, text.len(), 10);
        assert_eq!(m.rows, 2);
        assert_eq!(m.cursor_row, 1);
        assert_eq!(m.cursor_col, 5); // "89abc" is 5 wide, no prefix on row 1

        // Cursor exactly at the first-chunk boundary (byte 8) — the block
        // cursor covers '8', the first char of the continuation row.
        let m = input_metrics(text, 8, 10);
        assert_eq!(m.cursor_row, 1);
        assert_eq!(m.cursor_col, 0);

        // One char past the boundary → row 1, column 1.
        let m = input_metrics(text, 9, 10);
        assert_eq!(m.cursor_row, 1);
        assert_eq!(m.cursor_col, 1);
    }

    #[test]
    fn test_input_metrics_multi_line() {
        let m = input_metrics("ab\ncd", 5, 80);
        assert_eq!(m.rows, 2);
        assert_eq!(m.cursor_row, 1);
        assert_eq!(m.cursor_col, 4); // 2 prefix + "cd"

        // Cursor on the first line.
        let m = input_metrics("ab\ncd", 1, 80);
        assert_eq!(m.cursor_row, 0);
        assert_eq!(m.cursor_col, 3);
    }

    #[test]
    fn test_input_metrics_trailing_newline() {
        // "a\n" = line "a" + empty line — the cursor at the end sits on the
        // empty second row.
        let m = input_metrics("a\n", 2, 80);
        assert_eq!(m.rows, 2);
        assert_eq!(m.cursor_row, 1);
        assert_eq!(m.cursor_col, 2);
    }

    #[test]
    fn test_input_metrics_cjk() {
        // 你好世界 = 8 display cols; width 4 → first chunk 2 → "你" | "好世" | "界".
        let text = "你好世界";
        let m = input_metrics(text, text.len(), 4);
        assert_eq!(m.rows, 3);
        assert_eq!(m.cursor_row, 2);
        assert_eq!(m.cursor_col, 2); // "界" is 2 wide, no prefix on row 2
    }

    #[test]
    fn test_build_input_lines_matches_metrics() {
        // Rendering must produce exactly as many rows as input_metrics counts.
        let style = Style::default();
        let cases = [
            ("", 0usize, 80u16),
            ("hello", 5, 80),
            ("0123456789abc", 13, 10),
            ("0123456789abc", 8, 10),
            ("0123456789abc", 9, 10),
            ("0123456789abcdef", 3, 10), // cursor in first chunk of a 2-chunk line
            ("ab\ncd", 5, 80),
            ("a\n", 2, 80),
            ("你好世界", 12, 4),
        ];
        for (text, cursor, width) in cases {
            let m = input_metrics(text, cursor, width);
            let lines = build_input_lines(text, cursor, style, width);
            assert_eq!(lines.len(), m.rows, "rows mismatch for {text:?}");
            // Every row fits the width (prefix row: 2 + text ≤ width).
            for (i, line) in lines.iter().enumerate() {
                let total: usize = line
                    .spans
                    .iter()
                    .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                    .sum();
                assert!(
                    total <= width as usize,
                    "row {i} too wide: {total} > {width}"
                );
            }
        }
    }

    #[test]
    fn test_build_input_lines_highlights_cursor() {
        let style = Style::default().bg(ratatui::style::Color::Red);
        let lines = build_input_lines("hello", 2, style, 80);
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        // ["> " prefix, "he", highlighted "l", "lo"]
        assert_eq!(spans.len(), 4);
        assert_eq!(spans[2].style.bg, Some(ratatui::style::Color::Red));
        assert_eq!(spans[2].content.as_ref(), "l");
    }

    #[test]
    fn test_build_input_lines_wrapped_cursor_row() {
        let style = Style::default();
        // Cursor at the end of a wrapped line: trailing-space cursor on row 1.
        let lines = build_input_lines("0123456789abc", 13, style, 10);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans[1].content.as_ref(), "01234567");
        assert_eq!(lines[1].spans[0].content.as_ref(), "89abc");
        // Row 1 ends with the cursor space.
        assert_eq!(lines[1].spans.last().unwrap().content.as_ref(), " ");
    }

    #[test]
    fn test_build_input_lines_cursor_in_non_terminal_wrapped_chunk() {
        // Regression: a cursor in an early chunk must NOT bleed into later
        // chunks of the same wrapped line (missing lower-bound check caused
        // `line[offset..cursor]` slice panics and phantom cursor highlights).
        let style = Style::default().bg(ratatui::style::Color::Red);
        // width=10 → first_cap=8, w=10. 16 chars → two chunks:
        // "01234567" + "89abcdef". Cursor at byte 3 belongs to chunk 1 only.
        let lines = build_input_lines("0123456789abcdef", 3, style, 10);
        assert_eq!(lines.len(), 2);
        // Row 0: prefix, "012", highlighted "3", "4567".
        assert_eq!(lines[0].spans.len(), 4, "first chunk should have cursor");
        assert_eq!(lines[0].spans[2].content.as_ref(), "3");
        assert_eq!(lines[0].spans[2].style.bg, Some(ratatui::style::Color::Red));
        // Row 1 must be entirely plain — no cursor highlight.
        let second_has_cursor = lines[1]
            .spans
            .iter()
            .any(|s| s.style.bg == Some(ratatui::style::Color::Red));
        assert!(!second_has_cursor, "second chunk should not have cursor");
    }

    #[test]
    fn test_input_metrics_cjk_cursor_col_with_prefix() {
        // `cursor_col` includes the 2-col prompt prefix on row 0; the
        // hardware cursor formula adds only the 1-col border on top.
        let m = input_metrics("你好", "你好".len(), 80);
        assert_eq!(m.rows, 1);
        assert_eq!(m.cursor_row, 0);
        assert_eq!(m.cursor_col, 2 + 4); // 2 prefix + 4 CJK display cols

        // Cursor after "你" (2 display cols): 2 prefix + 2 = 4.
        let m = input_metrics("你好", "你".len(), 80);
        assert_eq!(m.cursor_col, 2 + 2);
    }

    #[test]
    fn test_build_input_lines_cjk_delete_scenario() {
        // Post-Delete state: was "你好世界" (12 bytes), Delete at byte 3
        // removed "好", leaving "你世界" (9 bytes) with the cursor still at
        // byte 3 — now on "世". The cursor highlight must land on "世".
        let style = Style::default().bg(ratatui::style::Color::Red);
        let lines = build_input_lines("你世界", 3, style, 80);
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        // prefix, "你", highlighted "世", "界"
        assert_eq!(spans.len(), 4);
        assert_eq!(spans[2].content.as_ref(), "世");
        assert_eq!(spans[2].style.bg, Some(ratatui::style::Color::Red));
        assert_eq!(spans[3].content.as_ref(), "界");
    }

    #[test]
    fn test_build_input_lines_wide_char_trailing_ghost_guard() {
        // Regression: deleting the last CJK char leaves the wide char's
        // right-half cell unchanged in the buffer, so ratatui's diff skips
        // it and the terminal shows a ghost half-character under the cursor
        // block. The trailing-space cursor must restyle that cell (visually
        // blank) to force a redraw. ASCII (1-col) trailing cursors stay one
        // cell — no guard needed.
        let style = Style::default().bg(ratatui::style::Color::Red);

        // CJK end-of-line: cursor space + ghost-guard cell.
        let lines = build_input_lines("你好世", 9, style, 80);
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 4); // prefix, 你好世, cursor " ", guard " "
        assert_eq!(spans[2].content.as_ref(), " ");
        assert_eq!(spans[2].style.bg, Some(ratatui::style::Color::Red));
        assert_eq!(spans[3].content.as_ref(), " ");
        // The guard is visually blank (no bg) but carries a fg so the
        // buffer cell differs from the plain " " it replaces.
        assert_eq!(spans[3].style.bg, None);
        assert_ne!(spans[3].style.fg, None);

        // ASCII end-of-line: cursor space only, no guard.
        let lines = build_input_lines("hell", 4, style, 80);
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 3); // prefix, hell, cursor " "
        assert_eq!(spans[2].content.as_ref(), " ");

        // Cursor mid-string (on a CJK char): no trailing space at all.
        let lines = build_input_lines("你好世界", 3, style, 80);
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 4); // prefix, 你, cursor 好, 世界
    }

    #[test]
    fn test_cjk_delete_clears_wide_char_trailing_cell() {
        use memory::PendingHints;
        use observability::TraceStore;
        use persistence::PersistenceConfig;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use std::path::PathBuf;
        use std::sync::{Arc, RwLock};

        fn make_app() -> App {
            let memory = Arc::new(RwLock::new(memory::Memory::new()));
            let pending_hints = PendingHints::default();
            let todos = Arc::new(RwLock::new(Vec::<crate::tools::TodoItem>::new()));
            let trace_store = Arc::new(TraceStore::new());
            let plan_mode = Arc::new(crate::hooks::PlanModeState::default());
            let plan_dir = PathBuf::from(".loomis/plan");
            let skill_registry = Arc::new(skills::SkillRegistry::empty());
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

        // Full-frame check: rendering "你好世界" with the cursor before the
        // last char, then deleting it, must leave the cell to the right of
        // the trailing cursor space changed — ratatui's diff redraws it and
        // the terminal clears the wide char's ghost right half.
        let mut app = make_app();
        app.input = "你好世界".into();
        app.input_cursor = 9; // cursor before "界"
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let before = terminal.backend().buffer().cell((9, 19)).unwrap().clone();

        // Simulate Delete at cursor 9 (drain bytes 9..12 = "界").
        // cursor stays 9 = end of "你好世".
        app.input.drain(9..12);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let after_cursor = buf.cell((9, 19)).unwrap().clone();
        let after_right = buf.cell((10, 19)).unwrap().clone();

        // Cursor cell: was the wide char, now the trailing space.
        assert_eq!(after_cursor.symbol(), " ");
        assert!(after_cursor.style().bg.is_some());
        // Right-half cell must differ from the pre-delete frame so the
        // diff redraws it (previously identical → ghost left behind).
        assert_ne!(
            after_right.style().fg,
            before.style().fg,
            "wide char's right-half cell must change to force a redraw"
        );
        assert_eq!(after_right.symbol(), " ");
        assert_ne!(
            after_right.style().bg,
            Some(ratatui::style::Color::Red),
            "guard cell stays visually blank (no cursor background)"
        );
    }

    #[test]
    fn test_split_at_display_width_empty() {
        assert_eq!(split_at_display_width("", 80), ("", ""));
    }

    #[test]
    fn test_split_at_display_width_fits() {
        let (before, after) = split_at_display_width("hello", 80);
        assert_eq!(before, "hello");
        assert_eq!(after, "");
    }

    #[test]
    fn test_split_at_display_width_exact() {
        // "abcde" is 5 chars, each 1 column
        let (before, after) = split_at_display_width("abcde", 5);
        assert_eq!(before, "abcde");
        assert_eq!(after, "");
    }

    #[test]
    fn test_split_at_display_width_overflow() {
        // 10 chars at width 5 → first 5 then remaining 5
        let (before, after) = split_at_display_width("abcdefghij", 5);
        assert_eq!(before, "abcde");
        assert_eq!(after, "fghij");
    }

    #[test]
    fn test_split_at_display_width_cjk() {
        // Chinese chars are 2 columns each
        let (before, after) = split_at_display_width("你好世界", 4);
        assert_eq!(before, "你好"); // 4 columns
        assert_eq!(after, "世界"); // 4 columns
    }

    #[test]
    fn test_split_at_display_width_narrow() {
        // Force at least one char even if it doesn't fit
        let (before, after) = split_at_display_width("hello", 1);
        assert_eq!(before, "h");
        assert_eq!(after, "ello");
    }

    #[test]
    fn test_wrap_to_width_no_wrap_needed() {
        let lines = vec![Line::from(Span::raw("short"))];
        let wrapped = wrap_to_width(lines, 80);
        assert_eq!(wrapped.len(), 1);
    }

    #[test]
    fn test_wrap_to_width_wraps_long_line() {
        // 20 chars at width 5 → 4 lines of 5 chars each
        let lines = vec![Line::from(Span::raw("abcdefghijklmnopqrst"))];
        let wrapped = wrap_to_width(lines, 5);
        assert_eq!(wrapped.len(), 4);
        assert_eq!(wrapped[0].spans[0].content, "abcde");
        assert_eq!(wrapped[1].spans[0].content, "fghij");
        assert_eq!(wrapped[2].spans[0].content, "klmno");
        assert_eq!(wrapped[3].spans[0].content, "pqrst");
    }

    #[test]
    fn test_wrap_to_width_cjk() {
        // 6 Chinese chars = 12 columns. At width 4 → 3 lines of 2 chars each.
        let lines = vec![Line::from(Span::raw("你好世界测试"))];
        let wrapped = wrap_to_width(lines, 4);
        assert_eq!(wrapped.len(), 3);
        // Each Chinese char is 2 columns, so 2 chars = 4 columns per line
        assert_eq!(wrapped[0].spans[0].content, "你好");
        assert_eq!(wrapped[1].spans[0].content, "世界");
        assert_eq!(wrapped[2].spans[0].content, "测试");
    }

    #[test]
    fn test_wrap_to_width_mixed_lines() {
        let lines = vec![
            Line::from(Span::raw("short")),
            Line::from(Span::raw("abcdefghij")), // 10 chars
        ];
        let wrapped = wrap_to_width(lines, 5);
        // "short" fits, "abcdefghij" → 2 lines
        assert_eq!(wrapped.len(), 3);
    }

    // ── tool_resource_summary tests ──────────────────────────────

    #[test]
    fn test_tool_resource_summary_read() {
        assert_eq!(
            tool_resource_summary("read", r#"{"file_path": "src/main.rs"}"#),
            Some("src/main.rs".into())
        );
    }

    #[test]
    fn test_tool_resource_summary_shell() {
        assert_eq!(
            tool_resource_summary("shell", r#"{"command": "cargo build", "timeout_secs": 60}"#),
            Some("cargo build".into())
        );
    }

    #[test]
    fn test_tool_resource_summary_grep() {
        assert_eq!(
            tool_resource_summary(
                "grep",
                r#"{"pattern": "fn main", "path_glob": "src/**/*.rs"}"#
            ),
            Some("fn main".into())
        );
    }

    #[test]
    fn test_tool_resource_summary_ls_with_path() {
        assert_eq!(
            tool_resource_summary("ls", r#"{"path": "src/"}"#),
            Some("src/".into())
        );
    }

    #[test]
    fn test_tool_resource_summary_ls_without_path() {
        assert_eq!(tool_resource_summary("ls", r#"{}"#), Some("root".into()));
    }

    #[test]
    fn test_tool_resource_summary_ls_null_path() {
        assert_eq!(
            tool_resource_summary("ls", r#"{"path": null}"#),
            Some("root".into())
        );
    }

    #[test]
    fn test_tool_resource_summary_calculator() {
        assert_eq!(
            tool_resource_summary("calculator", r#"{"expression": "2 + 3 * 4"}"#),
            Some("2 + 3 * 4".into())
        );
    }

    #[test]
    fn test_tool_resource_summary_echo() {
        assert_eq!(
            tool_resource_summary("echo", r#"{"text": "hello world"}"#),
            Some("hello world".into())
        );
    }

    #[test]
    fn test_tool_resource_summary_todo() {
        assert_eq!(
            tool_resource_summary(
                "todo",
                r#"{"todos": [{"content": "a", "status": "pending", "active_form": "A"}]}"#
            ),
            Some("1 items".into())
        );
    }

    #[test]
    fn test_tool_resource_summary_todo_multiple() {
        assert_eq!(
            tool_resource_summary("todo", r#"{"todos": [{}, {}, {}]}"#),
            Some("3 items".into())
        );
    }

    #[test]
    fn test_tool_resource_summary_empty_args() {
        assert_eq!(tool_resource_summary("read", ""), None);
    }

    #[test]
    fn test_tool_resource_summary_malformed_json() {
        assert_eq!(tool_resource_summary("read", "not json"), None);
    }

    #[test]
    fn test_tool_resource_summary_unknown_tool() {
        assert_eq!(tool_resource_summary("unknown_tool", r#"{"x": "y"}"#), None);
    }

    #[test]
    fn test_tool_resource_summary_truncation() {
        let long = "a".repeat(50);
        let json = format!(r#"{{"file_path": "{long}"}}"#);
        let result = tool_resource_summary("read", &json);
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.len() < 50);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn test_tool_resource_summary_write() {
        assert_eq!(
            tool_resource_summary(
                "write",
                r##"{"file_path": "output.md", "content": "# Hello"}"##
            ),
            Some("output.md".into())
        );
    }

    #[test]
    fn test_tool_resource_summary_glob() {
        assert_eq!(
            tool_resource_summary("glob", r#"{"pattern": "**/*.rs"}"#),
            Some("**/*.rs".into())
        );
    }

    #[test]
    fn test_tool_resource_summary_subagent() {
        assert_eq!(
            tool_resource_summary(
                "subagent",
                r#"{"description": "search for bugs", "prompt": "..."}"#
            ),
            Some("search for bugs".into())
        );
    }
}
