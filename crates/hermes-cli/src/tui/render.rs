//! Frame painter for the TUI.

use ratatui::layout::{Constraint, Direction, Layout, Position};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;
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

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),             // chat scrollback
            Constraint::Length(activity_h), // activity indicator (busy only)
            Constraint::Length(1),          // status row
            Constraint::Length(3),          // input block
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
    let input_text = build_input_line(app);
    let input = Paragraph::new(input_text)
        .block(input_block)
        .wrap(Wrap { trim: false });
    f.render_widget(input, chunks[3]);

    // --- Cursor: position it at the end of the typed text inside the input --
    // The input block is 3 rows tall; the cursor sits on the middle row, just
    // after the "❯ " prompt and the typed text.
    let input_x = chunks[3].x;
    let input_y = chunks[3].y;
    let cursor_x = input_x + 1 + visible_width("❯ ") as u16 + visible_width(&app.input) as u16;
    let cursor_y = input_y + 1;
    f.set_cursor_position(Position::new(cursor_x, cursor_y));
}

/// Build the chat-area `Vec<Line>` from the scrollback. Each line is
/// pre-wrapped to `width` columns so the line count matches the rendered
/// row count exactly. The assistant content lives inside a rounded
/// `╭─ Hermes ─...─╮` block (also pre-wrapped by `assistant_block`).
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
/// Top: `╭─ ⚕ Hermes ✦ ─...─╮`, body: `  <wrapped text>`, bottom: `╰─...─╯`.
fn assistant_block(text: &str, width: u16) -> Vec<Line<'static>> {
    let w = width.max(1) as usize;
    let body_indent = "  ";
    let inner_w = w.saturating_sub(visible_width(body_indent)).max(1);
    let title = " ⚕ Hermes ✦ ";
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
        let prefix = if idx == 0 { "… " } else { "  " };
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
        out.push(Line::from("…").dim());
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

    Line::from(spans)
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

/// Build the input line: `❯ {text_or_placeholder}`.
fn build_input_line(app: &App) -> Line<'static> {
    let prompt = Span::styled(
        "❯ ",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    if app.input.is_empty() {
        Line::from(vec![
            prompt,
            Span::styled(
                "Send a message…",
                Style::default().add_modifier(Modifier::DIM),
            ),
        ])
    } else {
        Line::from(vec![prompt, Span::raw(app.input.clone())])
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
        let app = App::new_for_test();
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
        let mut app = App::new_for_test();
        app.context_window_size = Some(1_000_000);
        app.context_used_tokens = Some(200_000);
        let line = build_status_line_1(&app);
        let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(s.contains("20%"), "expected 20% in status; got {s:?}");
    }

    #[test]
    fn status_line_uses_reported_context_usage() {
        let mut app = App::new_for_test();
        app.context_window_size = Some(1_000_000);
        app.context_used_tokens = Some(1_000);
        let line = build_status_line_1(&app);
        let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();

        assert!(s.contains("1.0K"), "expected 1.0K in status; got {s:?}");
        assert!(s.contains("<1%"), "expected <1% in status; got {s:?}");
    }
}
