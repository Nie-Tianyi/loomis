//! # Rendering
//!
//! All ratatui drawing for the three-panel layout: chat area, input area,
//! and status bar.

use std::sync::atomic::Ordering;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use engine::CallOrigin;
use serde_json;

use super::app::App;
use super::markdown::render_markdown;
use super::messages::{ChatMessage, ThreadPicker, ToolCallState};

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
        Constraint::Fill(1),   // chat
        Constraint::Length(5), // input (3 lines + border)
        Constraint::Length(1), // status bar
    ])
    .split(area);

    draw_chat(frame, layout[0], app);
    draw_input(frame, layout[1], app);
    draw_status(frame, layout[2], app);

    // ── Thread Picker Overlay ──────────────────────────────────
    if let Some(ref picker) = app.thread_picker {
        draw_thread_picker(frame, area, picker);
    }

    // Place the hardware cursor inside the input area.
    // Account for multi-line input (vertical offset) and CJK width.
    frame.set_cursor_position((
        layout[1].x + 2 + app.cursor_column(),
        layout[1].y + 1 + app.cursor_row(),
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
fn draw_chat(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Chat ")
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    let visible_height = inner.height.max(1) as usize;

    // Always reserve 1 column for the scrollbar so we only build lines
    // once. When no scrollbar is needed the extra column stays blank —
    // a negligible cost that eliminates dual-pass markdown rendering.
    let text_width = inner.width.saturating_sub(1).max(1);

    let raw_lines: Vec<Line<'_>> = app
        .messages
        .iter()
        .flat_map(|msg| message_to_lines(msg, text_width, app.intervene_selection))
        .collect();
    let all_lines = wrap_to_width(raw_lines, text_width);
    let total_lines = all_lines.len();
    let has_scrollbar = total_lines > visible_height;

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
                    "█",
                    Style::default().fg(Color::DarkGray),
                );
            } else {
                frame.buffer_mut().set_string(
                    scrollbar_area.x,
                    y,
                    "│",
                    Style::default()
                        .fg(Color::Rgb(60, 60, 60))
                        .add_modifier(Modifier::DIM),
                );
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
) -> Vec<Line<'_>> {
    // ── Timestamp style (shared across all variants) ───────────────
    let ts_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);

    match msg {
        ChatMessage::User { content, timestamp } => {
            let mut lines = Vec::new();
            let content_lines: Vec<&str> = content.lines().collect();
            for (i, line) in content_lines.iter().enumerate() {
                if i == 0 {
                    lines.push(Line::from(vec![
                        Span::styled(format!("{timestamp} "), ts_style),
                        Span::styled(
                            "> ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(*line, Style::default().fg(Color::White)),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::raw("       "), // align with timestamp + "> "
                        Span::styled(*line, Style::default().fg(Color::White)),
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
                .fg(Color::Yellow)
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
                                "$ ",
                                Style::default()
                                    .fg(Color::Green)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(args.as_str(), Style::default().fg(Color::Green)),
                        ]));
                        lines.push(Line::from(vec![
                            Span::raw("       "),
                            Span::styled(
                                "Running…",
                                Style::default()
                                    .fg(Color::Yellow)
                                    .add_modifier(Modifier::DIM),
                            ),
                        ]));
                    } else {
                        // Header: spinner + tool name + resource summary — yellow.
                        let resource = tool_resource_summary(name, args);
                        let mut header_spans = vec![
                            Span::styled(format!("{timestamp} "), ts_style),
                            Span::styled(
                                "◌ ",
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
                        ];
                        // Show the primary resource inline when available,
                        // e.g. "◌ read src/main.rs (running…)"
                        if let Some(ref res) = resource {
                            header_spans.push(Span::raw(" "));
                            header_spans.push(Span::styled(
                                res.to_owned(),
                                Style::default()
                                    .fg(Color::White)
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
                                    .fg(Color::Yellow)
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
                                "$ ",
                                Style::default()
                                    .fg(Color::Green)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(args.as_str(), Style::default().fg(Color::Green)),
                        ]));
                        // Multi-line output — dim gray, like old ShellOutput.
                        for line in output.lines() {
                            lines.push(Line::from(vec![
                                Span::raw("       "),
                                Span::styled(
                                    line,
                                    Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
                                ),
                            ]));
                        }
                    } else {
                        // Header: checkmark + tool name — green.
                        lines.push(Line::from(vec![
                            Span::styled(format!("{timestamp} "), ts_style),
                            Span::styled(
                                "✓ ",
                                Style::default()
                                    .fg(Color::Green)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                name.as_str(),
                                Style::default()
                                    .fg(Color::Green)
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
                                            .fg(Color::Gray)
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
                            "⊘ ",
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
                    ]));
                    // Rejection reason — yellow, dimmed.
                    let preview = truncate_output(reason, area_width);
                    if !preview.is_empty() {
                        lines.push(Line::from(vec![
                            Span::raw("       "),
                            Span::styled(
                                preview,
                                Style::default()
                                    .fg(Color::Yellow)
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
                            "✗ ",
                            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            name.as_str(),
                            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    // Error message — red, dimmed.
                    let preview = truncate_output(error, area_width);
                    if !preview.is_empty() {
                        lines.push(Line::from(vec![
                            Span::raw("       "),
                            Span::styled(
                                preview,
                                Style::default().fg(Color::Red).add_modifier(Modifier::DIM),
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
                                "ℹ ",
                                Style::default()
                                    .fg(Color::Magenta)
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
                        "✓ ",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(title.as_str(), Style::default().fg(Color::Yellow)),
                    Span::raw(" "),
                    Span::styled(summary, Style::default().fg(Color::White)),
                ]));
            } else {
                // Title
                lines.push(Line::from(vec![
                    Span::styled(format!("{timestamp} "), ts_style),
                    Span::styled(
                        "⚡ ",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        title.as_str(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                // Description
                for desc_line in description.lines() {
                    lines.push(Line::from(vec![
                        Span::raw("       "),
                        Span::styled(desc_line, Style::default().fg(Color::Cyan)),
                    ]));
                }
                // Options — each on its own line, highlighted when selected.
                if !options.is_empty() {
                    for (i, opt) in options.iter().enumerate() {
                        let is_selected = intervene_selection == Some(i);
                        let (prefix, style) = if is_selected {
                            (
                                "  ▶ ",
                                Style::default()
                                    .fg(Color::Yellow)
                                    .add_modifier(Modifier::BOLD),
                            )
                        } else {
                            (
                                "    ",
                                Style::default()
                                    .fg(Color::DarkGray)
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
            let error_style = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
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

    let (style, title) = if has_intervene && app.intervene_text_mode {
        (Style::default().fg(Color::Magenta), " Answer ")
    } else if has_intervene {
        (Style::default().fg(Color::Magenta), " Choose ")
    } else if app.streaming {
        (Style::default().fg(Color::Yellow), " Inject ")
    } else {
        (Style::default().fg(Color::Cyan), " Input ")
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(style);

    let cursor_style = if app.streaming {
        Style::default().bg(Color::DarkGray).fg(Color::White)
    } else {
        Style::default().bg(Color::White).fg(Color::Black)
    };

    let display_lines = build_input_lines(app, cursor_style);

    // Show a hint when the input is empty
    let lines: Vec<Line<'_>> = if app.input.is_empty() && has_intervene && app.intervene_text_mode {
        vec![
            Line::from(Span::raw(" ")),
            Line::from(Span::styled(
                " Type your response and press Enter. Esc to go back.",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )),
        ]
    } else if app.input.is_empty() && has_intervene {
        vec![
            Line::from(Span::raw(" ")),
            Line::from(Span::styled(
                " ↑↓ to navigate  ·  Enter to select  ·  Esc to cancel",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )),
        ]
    } else if app.input.is_empty() && app.streaming {
        vec![
            Line::from(Span::raw(" ")),
            Line::from(Span::styled(
                " Type to inject a hint while the agent is running. Enter to send.",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )),
        ]
    } else if app.input.is_empty() && !app.streaming {
        vec![
            Line::from(Span::raw(" ")),
            Line::from(Span::styled(
                " Type a message and press Enter. Shift+Enter for newline. /help for commands.",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )),
        ]
    } else {
        display_lines
    };

    // Clear residual characters from previous frame before rendering.
    frame.render_widget(Clear, area);

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

/// Builds the styled input lines: `"> "` prompt prefix on each line,
/// with cursor highlighting on the active line.
fn build_input_lines(app: &App, cursor_style: Style) -> Vec<Line<'_>> {
    let input = &app.input;
    let cursor = app.input_cursor;
    let mut lines: Vec<Line<'_>> = Vec::new();

    // Find which line the cursor is on
    let cursor_line_idx = input[..cursor].chars().filter(|&c| c == '\n').count();

    for (line_idx, text_line) in input.lines().enumerate() {
        let line_start = input
            .lines()
            .take(line_idx)
            .map(|l| l.len() + 1) // +1 for the \n
            .sum::<usize>();

        let mut spans: Vec<Span<'_>> = Vec::new();

        // Prompt prefix on every line
        spans.push(Span::styled(
            "> ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

        if line_idx == cursor_line_idx {
            // This line contains the cursor
            let cursor_in_line = cursor - line_start;

            // Text before cursor
            if cursor_in_line > 0 && cursor_in_line <= text_line.len() {
                spans.push(Span::raw(&text_line[..cursor_in_line.min(text_line.len())]));
            }

            // Cursor character
            if cursor_in_line < text_line.len() {
                let ch = text_line[cursor_in_line..].chars().next().unwrap_or(' ');
                spans.push(Span::styled(ch.to_string(), cursor_style));
                let after_start = cursor_in_line + ch.len_utf8();
                if after_start < text_line.len() {
                    spans.push(Span::raw(&text_line[after_start..]));
                }
            } else {
                // Cursor at end of line — show a space
                spans.push(Span::styled(" ", cursor_style));
            }
        } else {
            // Plain text line
            spans.push(Span::raw(text_line));
        }

        lines.push(Line::from(spans));
    }

    if lines.is_empty() {
        // No content — show cursor on empty first line
        lines.push(Line::from(vec![
            Span::styled(
                "> ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", cursor_style),
        ]));
    }

    lines
}

// ── Status Bar ───────────────────────────────────────────────────────────────────

/// Renders the single-line status bar at the bottom with better styling.
fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let bg_color = Color::Rgb(30, 40, 50);
    let fg_color = Color::Rgb(180, 190, 200);
    let accent = if app.streaming {
        Color::Rgb(255, 180, 50)
    } else {
        Color::Rgb(80, 200, 120)
    };

    let (left, right, accent_text) = build_status_content(app);

    let left_width = UnicodeWidthStr::width(left.as_str());
    let accent_width = UnicodeWidthStr::width(accent_text.as_str());
    let right_width = UnicodeWidthStr::width(right.as_str());
    let total_space = area.width as usize;

    let gap = if left_width + accent_width + right_width < total_space {
        total_space - left_width - accent_width - right_width
    } else {
        1
    };

    let line = Line::from(vec![
        Span::styled(left, Style::default().fg(fg_color).bg(bg_color)),
        Span::styled(" ".repeat(gap), Style::default().fg(fg_color).bg(bg_color)),
        Span::styled(accent_text, Style::default().fg(accent).bg(bg_color)),
        Span::styled(right, Style::default().fg(Color::DarkGray).bg(bg_color)),
    ]);

    // Clear residual characters from previous frame before rendering.
    frame.render_widget(Clear, area);

    let paragraph = Paragraph::new(line);
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
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Rgb(20, 25, 35)));

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
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
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
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    ));
    lines.push(footer);

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

/// Builds the left, accent, and right portions of the status bar.
fn build_status_content(app: &App) -> (String, String, String) {
    let model = &app.model;
    let msgs = app.messages.len();

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
            format!("#{steps} · {llm} LLM · {tools} tools · {tokens} | ")
        } else {
            String::new()
        }
    };

    // Plan mode indicator
    let plan_part = if app.plan_mode.active.load(Ordering::SeqCst) {
        " PLAN | ".to_string()
    } else {
        String::new()
    };

    let left = format!(" {plan_part}{todo_part}{trace_part}{model} | {msgs} msgs ");

    let (accent, right) = if app.streaming {
        let indicator = " ⚡ STREAMING ";
        let right = format!(
            "PgUp/Dn/🖱 scroll{}  ",
            if app.scroll_offset > 0 {
                format!(" ↑{}", app.scroll_offset)
            } else {
                String::new()
            }
        );
        (indicator.to_string(), right)
    } else {
        let right = format!(
            "{}  ",
            if app.scroll_offset > 0 {
                format!("↑{} scrolled", app.scroll_offset)
            } else {
                String::new()
            }
        );
        (String::new(), right)
    };

    (left, accent, right)
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

    /// Returns the visual column of the cursor within the input line,
    /// accounting for CJK double-width characters.
    pub fn cursor_column(&self) -> u16 {
        let prefix_len = 2u16; // "> " prompt

        // For multi-line input, find the start of the current line
        let line_start = self.input[..self.input_cursor]
            .rfind('\n')
            .map(|p| p + 1)
            .unwrap_or(0);

        let col = if self.input_cursor <= line_start {
            0
        } else {
            UnicodeWidthStr::width(&self.input[line_start..self.input_cursor]) as u16
        };
        prefix_len + col
    }

    /// Returns the row offset of the cursor within a multi-line input
    /// (0 = first line). Used for vertical cursor placement.
    pub fn cursor_row(&self) -> u16 {
        self.input[..self.input_cursor]
            .chars()
            .filter(|&c| c == '\n')
            .count() as u16
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

    #[test]
    fn test_cursor_column_no_prompt() {
        // The cursor_column method includes the "> " prefix
        // This is tested via the App struct
    }

    // ── wrap_to_width / split_at_display_width tests ────────────

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
