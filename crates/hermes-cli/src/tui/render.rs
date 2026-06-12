//! Frame painter for the TUI.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Position};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::app::App;
use crate::tui::event::{AppMode, RenderedLine};

/// Paint one frame.
pub fn render(f: &mut Frame, app: &mut App) {
    let activity_h = if matches!(app.mode, AppMode::AwaitingModel | AppMode::Cancelling) {
        1
    } else {
        0
    };

    // Input block: grows to fit the wrapped input text, up to a maximum
    // visible content of `MAX_INPUT_CONTENT_LINES`. The minimum is
    // `MIN_INPUT_CONTENT_LINES` so the box never shrinks below the
    // legacy 3-line input area.
    let full_inner_w = f.area().width.saturating_sub(2).max(1);
    let input_lines = build_input_lines(app, full_inner_w);
    let content_lines = input_lines.len();
    let visible_content_lines =
        content_lines.clamp(MIN_INPUT_CONTENT_LINES, MAX_INPUT_CONTENT_LINES);
    let input_h: u16 = (visible_content_lines + 2) as u16;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),             // chat scrollback
            Constraint::Length(activity_h), // activity indicator (busy only)
            Constraint::Length(1),          // status row
            Constraint::Length(input_h),    // input block
        ])
        .split(f.area());

    // --- Chat scrollback ----------------------------------------------------
    let chat_area = chunks[0];
    let chat_scroll = app.chat_scroll;
    let chat_lines = app.chat_lines_for_width(chat_area.width);
    let total_lines = chat_lines.len() as u16;
    let visible_h = chat_area.height;
    let max_scroll = total_lines.saturating_sub(visible_h);
    let effective_scroll = chat_scroll.min(max_scroll);
    let scroll_y = total_lines
        .saturating_sub(visible_h)
        .saturating_sub(effective_scroll);
    let chat = Paragraph::new(chat_lines.to_vec())
        .block(Block::default().borders(Borders::NONE))
        .scroll((scroll_y, 0));
    f.render_widget(chat, chat_area);

    // --- Activity indicator (only when busy) --------------------------------
    if matches!(app.mode, AppMode::AwaitingModel | AppMode::Cancelling) {
        let activity = build_activity_line(app);
        let activity_widget = Paragraph::new(activity)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::NONE));
        f.render_widget(activity_widget, chunks[1]);
    }

    // --- Status row (always visible metadata) --------------------------------
    let status_line = build_status_line_1(app);
    let status = Paragraph::new(status_line)
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::NONE));
    f.render_widget(status, chunks[2]);

    // --- Input block --------------------------------------------------------
    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);

    // Inner area (inside the border).
    let inner_w = chunks[3].width.saturating_sub(2);
    let inner_h = chunks[3].height.saturating_sub(2);

    // Scroll the input so the cursor stays visible when the wrapped text
    // exceeds the visible area.
    let cursor_row = compute_input_cursor_row(app, inner_w);
    let scroll_y: u16 = if cursor_row >= inner_h {
        cursor_row - inner_h + 1
    } else {
        0
    };

    let input = Paragraph::new(input_lines)
        .block(input_block)
        .scroll((scroll_y, 0));
    f.render_widget(input, chunks[3]);

    // --- Cursor: position at the cursor byte offset, accounting for wrap ----
    let inner_x = chunks[3].x + 1;
    let inner_y = chunks[3].y + 1;

    let (cursor_col, visible_cursor_row) = compute_input_cursor_col_row(app, inner_w);
    let max_visible_row = inner_h.saturating_sub(1);
    let cursor_y = inner_y + visible_cursor_row.min(max_visible_row);
    let cursor_x = inner_x + cursor_col.min(inner_w.saturating_sub(1));

    f.set_cursor_position(Position::new(cursor_x, cursor_y));
}

/// Paint only the fixed bottom viewport. Chat history is written once into
/// terminal scrollback by the run loop and is not part of this frame.
pub fn render_bottom(f: &mut Frame, app: &App) {
    let area = f.area();
    let input_lines = build_input_lines(app, area.width.saturating_sub(2).max(1));
    let desired_h = bottom_view_height(app, area.width).min(area.height);
    let y = area.y + area.height.saturating_sub(desired_h);
    let area = ratatui::layout::Rect {
        x: area.x,
        y,
        width: area.width,
        height: desired_h,
    };
    f.render_widget(Clear, area);
    let input_h = bottom_input_height(input_lines.len()).min(area.height.saturating_sub(1));
    let activity_h = if matches!(app.mode, AppMode::AwaitingModel | AppMode::Cancelling) {
        1
    } else {
        0
    };
    let fixed_h = activity_h + 1 + input_h;
    let top_padding_h = area.height.saturating_sub(fixed_h);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(top_padding_h),
            Constraint::Length(activity_h),
            Constraint::Length(1),
            Constraint::Length(input_h),
        ])
        .split(area);

    if matches!(app.mode, AppMode::AwaitingModel | AppMode::Cancelling) {
        let activity = build_activity_line(app);
        let activity_widget = Paragraph::new(activity)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::NONE));
        f.render_widget(activity_widget, chunks[1]);
    }

    let status_line = build_status_line_1(app);
    let status = Paragraph::new(status_line)
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::NONE));
    f.render_widget(status, chunks[2]);

    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
    let inner_w = chunks[3].width.saturating_sub(2);
    let inner_h = chunks[3].height.saturating_sub(2);
    let cursor_row = compute_input_cursor_row(app, inner_w);
    let scroll_y: u16 = if cursor_row >= inner_h {
        cursor_row - inner_h + 1
    } else {
        0
    };

    let input = Paragraph::new(input_lines)
        .block(input_block)
        .scroll((scroll_y, 0));
    f.render_widget(input, chunks[3]);

    let inner_x = chunks[3].x + 1;
    let inner_y = chunks[3].y + 1;
    let (cursor_col, visible_cursor_row) = compute_input_cursor_col_row(app, inner_w);
    let max_visible_row = inner_h.saturating_sub(1);
    let cursor_y = inner_y + visible_cursor_row.min(max_visible_row);
    let cursor_x = inner_x + cursor_col.min(inner_w.saturating_sub(1));
    f.set_cursor_position(Position::new(cursor_x, cursor_y));
}

pub fn bottom_view_height(app: &App, width: u16) -> u16 {
    let input_lines = build_input_lines(app, width.saturating_sub(2).max(1));
    let activity_h = if matches!(app.mode, AppMode::AwaitingModel | AppMode::Cancelling) {
        1
    } else {
        0
    };
    activity_h + 1 + bottom_input_height(input_lines.len())
}

fn bottom_input_height(input_line_count: usize) -> u16 {
    let visible_content_lines =
        input_line_count.clamp(MIN_INPUT_CONTENT_LINES, MAX_INPUT_CONTENT_LINES);
    (visible_content_lines + 2) as u16
}

/// Minimum number of content rows visible inside the input block. Default
/// is one line, since most chat input is short; the block grows as the
/// wrapped text requires.
const MIN_INPUT_CONTENT_LINES: usize = 1;
/// Maximum number of content rows visible inside the input block. When the
/// wrapped text is longer, the input scrolls so the cursor stays visible.
const MAX_INPUT_CONTENT_LINES: usize = 8;

/// Build the chat-area `Vec<Line>` from the scrollback. Each line is
/// pre-wrapped to `width` columns so the line count matches the rendered
/// row count exactly. The assistant content lives inside a rounded
/// `╭─ Perry Hermes ─...─╮` block (also pre-wrapped by `assistant_block`).
pub(crate) fn build_chat_lines(scrollback: &[RenderedLine], width: u16) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    for line in scrollback {
        match line {
            RenderedLine::User(s) => {
                let raw = format!("> {s}");
                out.extend(wrap_multiline_text(&raw, width));
            }
            RenderedLine::Assistant(s) => {
                out.extend(assistant_block(s, width));
            }
            RenderedLine::Reasoning(s) => {
                out.extend(reasoning_block(s, width));
            }
            RenderedLine::ToolCall { name, args_preview } => {
                let raw = format!("⚙ {name}({args_preview})");
                out.extend(wrap_multiline_text(&raw, width));
            }
            RenderedLine::ToolResult { name, output, ok } => {
                let glyph = if *ok { "✓" } else { "✗" };
                let raw = format!("{glyph} {name}: {output}");
                out.extend(wrap_multiline_text(&raw, width));
            }
            RenderedLine::System(s) => {
                let raw = format!("[system] {s}");
                out.extend(wrap_multiline_text(&raw, width));
            }
        }
    }
    out
}

fn wrap_multiline_text(text: &str, width: u16) -> Vec<Line<'static>> {
    let normalized = text.replace("\r\n", "\n");
    let mut out = Vec::new();
    for segment in normalized.split('\n') {
        out.extend(wrap_to_width(segment, width));
    }
    out
}

/// Hard-wrap a single string to a target column width. Returns one or more
/// lines that, when concatenated, contain the same text broken at word
/// boundaries. Overlong words are hard-split across multiple lines.
fn wrap_to_width(text: &str, width: u16) -> Vec<Line<'static>> {
    let w = width as usize;
    if w == 0 {
        return vec![Line::from(text.to_string())];
    }
    if visible_width(text) <= w {
        return vec![Line::from(text.to_string())];
    }
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let word_len = visible_width(word);
        if word_len > w {
            // Hard-split the overlong word across multiple lines.
            if !current.is_empty() {
                out.push(Line::from(std::mem::take(&mut current)));
            }
            let mut chunk = String::new();
            let mut chunk_width = 0usize;
            for ch in word.chars() {
                let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
                if chunk_width.saturating_add(ch_width) > w && !chunk.is_empty() {
                    out.push(Line::from(std::mem::take(&mut chunk)));
                    chunk_width = 0;
                }
                chunk.push(ch);
                chunk_width += ch_width;
            }
            if !chunk.is_empty() {
                current = chunk;
            }
            continue;
        }
        if current.is_empty() {
            current.push_str(word);
        } else if visible_width(&current) + 1 + word_len <= w {
            current.push(' ');
            current.push_str(word);
        } else {
            out.push(Line::from(std::mem::take(&mut current)));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        out.push(Line::from(current));
    }
    if out.is_empty() {
        out.push(Line::from(text.to_string()));
    }
    out
}

/// Render assistant text with a framed header/footer and plain indented body.
/// Top: `╭─ ⚕ Perry Hermes ✦ ─...─╮`, body: `  <wrapped text>`, bottom: `╰─...─╯`.
fn assistant_block(text: &str, width: u16) -> Vec<Line<'static>> {
    let w = width.max(1) as usize;
    let body_indent = "  ";
    let inner_w = w.saturating_sub(visible_width(body_indent)).max(1);
    let title = " ⚕ Perry Hermes ✦ ";
    let top_prefix = "╭─";
    let top_suffix = "╮";
    let top = fit_line_to_width(format!("{top_prefix}{title}{top_suffix}"), w, top_suffix);
    let bot_prefix = "╰─";
    let bot_suffix = "╯";
    let bot = fit_line_to_width(format!("{bot_prefix}{bot_suffix}"), w, bot_suffix);

    let mut out: Vec<Line<'static>> = Vec::new();
    out.push(Line::from(top).bold().cyan());

    let normalized = text.replace("\r\n", "\n");
    for segment in normalized.split('\n') {
        let wrapped = wrap_to_width(segment, inner_w as u16);
        for line in wrapped {
            let content = line
                .spans
                .first()
                .map(|span| span.content.as_ref())
                .unwrap_or("");
            out.push(Line::from(format!("{body_indent}{content}")));
        }
    }

    out.push(Line::from(bot).cyan());
    out
}

fn reasoning_block(text: &str, width: u16) -> Vec<Line<'static>> {
    let normalized = text.replace("\r\n", "\n");
    let mut out = Vec::new();
    for (idx, segment) in normalized.split('\n').enumerate() {
        let prefix = if idx == 0 { "✦ " } else { "  " };
        let wrapped = wrap_to_width(segment, width.saturating_sub(prefix.len() as u16));
        for (line_idx, line) in wrapped.into_iter().enumerate() {
            let content = line
                .spans
                .first()
                .map(|span| span.content.as_ref())
                .unwrap_or("");
            let head = if line_idx == 0 { prefix } else { "  " };
            out.push(Line::from(format!("{head}{content}")).dim());
        }
    }
    if out.is_empty() {
        out.push(Line::from("✦").dim());
    }
    out
}

/// Build the metadata row: `⚕ {provider} · {model} · {in_tok} / {total} {pct}% · {elapsed}`.
/// The context segment is omitted when `app.context_window_size` is `None`.
/// The elapsed segment is omitted when there is no active turn timer.
fn build_status_line_1(app: &App) -> Line<'static> {
    let provider = app.provider_name.as_deref().unwrap_or("?");
    let model = app.model_name.as_deref().unwrap_or("?");
    let elapsed = app
        .turn_started_at
        .map(|t| fmt_elapsed_compact(t.elapsed().as_secs()))
        .filter(|elapsed| !elapsed.is_empty());

    let mut spans: Vec<Span<'static>> = vec![
        Span::raw("⚕ "),
        Span::raw(provider.to_string()),
        Span::raw(" · "),
        Span::raw(model.to_string()),
    ];

    if let Some(total) = app.context_window_size {
        let in_tok = app.context_used_tokens.unwrap_or(0);
        spans.push(Span::raw(" · "));
        spans.push(Span::raw(format!(
            "{} / {}",
            format_tokens(in_tok),
            format_tokens(total)
        )));
        spans.push(Span::raw(format!(
            " {}",
            format_context_percent(in_tok, total)
        )));
    }

    if let Some(elapsed) = elapsed {
        spans.push(Span::raw(" · "));
        spans.push(Span::raw(elapsed));
    }

    if let Some(hint) = &app.compression_hint {
        spans.push(Span::raw(" · "));
        spans.push(Span::raw(hint.clone()));
    }

    if !app.pending_queue.is_empty() {
        let total = app.pending_queue.len();
        let last = app.pending_queue.last().map(String::as_str).unwrap_or("");
        let preview = truncate_for_status(last, 40);
        spans.push(Span::raw(" · "));
        if total == 1 {
            spans.push(Span::raw(format!("queued: {preview}")));
        } else {
            spans.push(Span::raw(format!("queued ({total}): {preview}")));
        }
    }

    Line::from(spans)
}

/// Truncate a string to at most `max_chars` characters, appending an
/// ellipsis when the original was longer. Operates on char boundaries
/// so CJK input is not split mid-codepoint.
fn truncate_for_status(s: &str, max_chars: usize) -> String {
    let mut out: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars {
        out.push('…');
    }
    out
}

/// Build the activity line shown while a turn is in flight or being cancelled.
fn build_activity_line(app: &App) -> Line<'static> {
    let elapsed = app
        .turn_started_at
        .map(|t| fmt_elapsed_compact(t.elapsed().as_secs()))
        .unwrap_or_else(|| "0s".to_string());
    match app.mode {
        AppMode::AwaitingModel => Line::from(vec![
            Span::raw(format!("{} ", spinner_frame())),
            Span::raw("Working").bold(),
            Span::raw(" · "),
            Span::raw(elapsed),
            Span::raw(" · Esc to stop").dim(),
        ]),
        AppMode::Cancelling => Line::from(vec![
            Span::raw("◌ "),
            Span::raw("Cancelling").bold(),
            Span::raw(" · "),
            Span::raw(elapsed),
        ]),
        AppMode::Idle => Line::default(),
    }
}

/// Build the input lines: the first line carries the cyan `❯ ` prompt, and
/// the rest of the text is hard-wrapped to the inner width so the cursor's
/// row/col match the rendered position. Returns one `Line` per visual row.
fn build_input_lines(app: &App, inner_w: u16) -> Vec<Line<'static>> {
    let prompt_span = Span::styled(
        "❯ ",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    let prompt_width = visible_width("❯ ");
    let inner_w_us = inner_w as usize;

    if app.input.is_empty() {
        return build_placeholder_lines(prompt_span, inner_w_us, prompt_width);
    }

    let first_w = inner_w_us.saturating_sub(prompt_width);
    if first_w == 0 {
        return vec![Line::from(prompt_span)];
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut remaining: &str = app.input.as_str();

    // First line: prompt + as much text as fits after the prompt.
    let (first, rest) = split_at_display_width(remaining, first_w);
    lines.push(Line::from(vec![prompt_span, Span::raw(first.to_string())]));
    remaining = rest;

    // Subsequent lines: full inner_w of text per row.
    while !remaining.is_empty() {
        let (segment, rest) = split_at_display_width(remaining, inner_w_us);
        lines.push(Line::from(segment.to_string()));
        remaining = rest;
    }

    lines
}

/// Build the placeholder lines shown when the input is empty. The
/// placeholder is rendered dim and wraps the same way as user input.
fn build_placeholder_lines(
    prompt_span: Span<'static>,
    inner_w: usize,
    prompt_width: usize,
) -> Vec<Line<'static>> {
    const PLACEHOLDER: &str = "Send a message…";
    let dim = Style::default().add_modifier(Modifier::DIM);
    let first_w = inner_w.saturating_sub(prompt_width);
    if first_w == 0 {
        return vec![Line::from(prompt_span)];
    }
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut remaining: &str = PLACEHOLDER;
    let (first, rest) = split_at_display_width(remaining, first_w);
    lines.push(Line::from(vec![
        prompt_span,
        Span::styled(first.to_string(), dim),
    ]));
    remaining = rest;
    while !remaining.is_empty() {
        let (segment, rest) = split_at_display_width(remaining, inner_w);
        lines.push(Line::from(Span::styled(segment.to_string(), dim)));
        remaining = rest;
    }
    lines
}

/// Split `s` so the prefix has display width `<= max_width` and the suffix
/// holds the rest. Splits at Unicode character boundaries.
fn split_at_display_width(s: &str, max_width: usize) -> (&str, &str) {
    if max_width == 0 {
        return ("", s);
    }
    let mut width = 0usize;
    let mut end = 0usize;
    for (i, c) in s.char_indices() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if width + cw > max_width {
            return (&s[..i], &s[i..]);
        }
        width += cw;
        end = i + c.len_utf8();
    }
    (&s[..end], &s[end..])
}

/// Return the 0-indexed visual row the cursor sits on within the wrapped
/// input. 0 means the first (prompt) line.
fn compute_input_cursor_row(app: &App, inner_w: u16) -> u16 {
    let prompt_width = visible_width("❯ ");
    let first_w = (inner_w as usize).saturating_sub(prompt_width);
    if first_w == 0 {
        return 0;
    }
    let text_before = &app.input[..app.cursor.min(app.input.len())];
    let text_before_width = visible_width(text_before);
    if text_before_width <= first_w {
        0
    } else {
        let after_first = text_before_width - first_w;
        let inner_w_us = inner_w.max(1) as usize;
        1 + (after_first / inner_w_us) as u16
    }
}

/// Return `(col, row)` for the cursor inside the inner input area. `col` is
/// 0-indexed from the inner left edge; `row` is 0-indexed from the inner
/// top edge.
fn compute_input_cursor_col_row(app: &App, inner_w: u16) -> (u16, u16) {
    let prompt_width = visible_width("❯ ") as u16;
    let first_w = (inner_w as usize).saturating_sub(prompt_width as usize);
    if first_w == 0 {
        return (0, 0);
    }
    let text_before = &app.input[..app.cursor.min(app.input.len())];
    let text_before_width = visible_width(text_before);
    if text_before_width <= first_w {
        (prompt_width + text_before_width as u16, 0)
    } else {
        let after_first = text_before_width - first_w;
        let inner_w_us = inner_w.max(1) as usize;
        let row = 1 + (after_first / inner_w_us);
        let col = after_first % inner_w_us;
        (col as u16, row as u16)
    }
}

/// Format elapsed seconds into a compact human-friendly form used by the
/// status line. Examples: `0s`, `59s`, `1m 00s`, `59m 59s`, `1h 00m 00s`,
/// `2h 03m 09s`.
pub fn fmt_elapsed_compact(elapsed_secs: u64) -> String {
    if elapsed_secs < 60 {
        return format!("{elapsed_secs}s");
    }
    if elapsed_secs < 3600 {
        let minutes = elapsed_secs / 60;
        let seconds = elapsed_secs % 60;
        return format!("{minutes}m {seconds:02}s");
    }
    let hours = elapsed_secs / 3600;
    let minutes = (elapsed_secs % 3600) / 60;
    let seconds = elapsed_secs % 60;
    format!("{hours}h {minutes:02}m {seconds:02}s")
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn format_context_percent(used: u64, total: u64) -> String {
    if total == 0 {
        return "0%".to_string();
    }
    let pct = ((used as f64 / total as f64) * 100.0).clamp(0.0, 100.0);
    if used > 0 && pct < 1.0 {
        "<1%".to_string()
    } else {
        format!("{pct:.0}%")
    }
}

fn visible_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn fit_line_to_width(mut base: String, width: usize, right_edge: &str) -> String {
    let edge_width = visible_width(right_edge);
    while visible_width(&base) + 1 + edge_width <= width {
        let insert_at = base.len().saturating_sub(right_edge.len());
        base.insert(insert_at, '─');
    }
    base
}

fn spinner_frame() -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let idx = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| ((elapsed.as_millis() / 80) % FRAMES.len() as u128) as usize)
        .unwrap_or(0);
    FRAMES[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_elapsed_compact_formats_seconds_minutes_hours() {
        assert_eq!(fmt_elapsed_compact(0), "0s");
        assert_eq!(fmt_elapsed_compact(1), "1s");
        assert_eq!(fmt_elapsed_compact(59), "59s");
        assert_eq!(fmt_elapsed_compact(60), "1m 00s");
        assert_eq!(fmt_elapsed_compact(61), "1m 01s");
        assert_eq!(fmt_elapsed_compact(3 * 60 + 5), "3m 05s");
        assert_eq!(fmt_elapsed_compact(59 * 60 + 59), "59m 59s");
        assert_eq!(fmt_elapsed_compact(3_600), "1h 00m 00s");
        assert_eq!(fmt_elapsed_compact(3_600 + 60 + 1), "1h 01m 01s");
        assert_eq!(fmt_elapsed_compact(25 * 3_600 + 2 * 60 + 3), "25h 02m 03s");
    }

    #[test]
    fn status_line_omits_context_when_unset() {
        let app = App::default();
        let line = build_status_line_1(&app);
        let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        // The "iter 0/0" portion also contains a slash, so the assertion is
        // specifically about the context segment: no "X.XK / Y.YK" pattern.
        assert!(
            !s.contains(" / 1.0M") && !s.contains("K /") && !s.contains("M /"),
            "expected no token count in status when context_window_size is None; got {s:?}"
        );
    }

    #[test]
    fn status_line_includes_context_percent_when_set() {
        let mut app = App::default();
        app.context_window_size = Some(1_000_000);
        app.context_used_tokens = Some(200_000);
        let line = build_status_line_1(&app);
        let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(s.contains("20%"), "expected 20% in status; got {s:?}");
    }

    #[test]
    fn status_line_uses_reported_context_usage() {
        let mut app = App::default();
        app.context_window_size = Some(1_000_000);
        app.context_used_tokens = Some(1_000);
        let line = build_status_line_1(&app);
        let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();

        assert!(s.contains("1.0K"), "expected 1.0K in status; got {s:?}");
        assert!(s.contains("<1%"), "expected <1% in status; got {s:?}");
    }

    #[test]
    fn status_line_shows_compression_hint() {
        let mut app = App::default();
        app.compression_hint = Some("Compressed in 1200ms".to_string());

        let line = build_status_line_1(&app);
        let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();

        assert!(
            s.contains("Compressed in 1200ms"),
            "expected compression hint in status; got {s:?}"
        );
    }

    #[test]
    fn status_line_shows_queued_messages_without_touching_scrollback() {
        let mut app = App::default();
        let scrollback_before = app.scrollback.len();
        app.pending_queue = vec!["可以了".to_string(), "再加一条".to_string()];

        let line = build_status_line_1(&app);
        let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();

        assert!(
            s.contains("queued (2):"),
            "expected queued count prefix; got {s:?}"
        );
        // The most recent message is shown as a preview; older ones
        // do not appear in the status line.
        assert!(
            s.contains("再加一条"),
            "expected latest queued message in status; got {s:?}"
        );
        assert!(
            !s.contains("可以了"),
            "older queued messages should not be in the status line; got {s:?}"
        );
        // Scrollback must not have grown — queued messages are
        // surfaced in the status bar only.
        assert_eq!(
            app.scrollback.len(),
            scrollback_before,
            "scrollback must not receive queued messages"
        );
    }

    #[test]
    fn status_line_truncates_long_queued_previews() {
        let mut app = App::default();
        app.pending_queue = vec!["a".repeat(80)];

        let line = build_status_line_1(&app);
        let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();

        assert!(
            s.contains('…'),
            "long queued message should be truncated with ellipsis; got {s:?}"
        );
        // The 41st 'a' (index 40) is the last kept, and an ellipsis
        // follows. So 40 a's in total appear, not 80.
        let a_count = s.chars().filter(|c| *c == 'a').count();
        assert!(
            a_count <= 40,
            "truncated preview should keep at most 40 chars of body; got {a_count}"
        );
    }

    #[test]
    fn truncate_for_status_handles_cjk_boundaries() {
        let truncated =
            truncate_for_status("你好世界这是一句长长长长长长长长长长长长长长长的话", 6);
        assert!(
            truncated.ends_with('…'),
            "CJK input longer than max should be truncated: {truncated:?}"
        );
        // The body before the ellipsis must be at most max_chars chars.
        let body = truncated.trim_end_matches('…');
        assert!(body.chars().count() <= 6);
    }
}
