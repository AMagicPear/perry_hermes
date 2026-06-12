//! One-shot formatting for chat history written into terminal scrollback.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::app::App;
use crate::tui::event::RenderedLine;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

#[derive(Debug, Default, Clone)]
pub struct HistoryWrite {
    pub lines: Vec<Line<'static>>,
    active_stream: Option<ActiveStream>,
}

impl HistoryWrite {
    pub fn push(&mut self, _app: &mut App, line: RenderedLine, width: u16) {
        self.finish_stream(width);
        self.lines.extend(format_history_line(&line, width));
    }

    pub fn drain(&mut self) -> Vec<Line<'static>> {
        std::mem::take(&mut self.lines)
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.active_stream = None;
    }

    pub fn push_assistant_delta(&mut self, text: &str, width: u16) {
        match &mut self.active_stream {
            Some(ActiveStream::Assistant(_)) => {}
            Some(ActiveStream::Reasoning(..)) => {
                self.finish_stream(width);
                self.lines.extend(assistant_header(width));
                self.active_stream = Some(ActiveStream::Assistant(String::new()));
            }
            None => {
                self.lines.extend(assistant_header(width));
                self.active_stream = Some(ActiveStream::Assistant(String::new()));
            }
        }
        if let Some(ActiveStream::Assistant(buffer)) = &mut self.active_stream {
            buffer.push_str(text);
            self.lines.extend(drain_stream(
                buffer,
                width,
                "  ",
                &mut true, // assistant never uses first-line prefix
                format_assistant_body_line,
                format_assistant_body_line,
            ));
        }
    }

    pub fn push_reasoning_delta(&mut self, text: &str, width: u16) {
        match &mut self.active_stream {
            Some(ActiveStream::Reasoning(buffer, first_line_emitted)) => {
                buffer.push_str(text);
                self.lines.extend(drain_stream(
                    buffer,
                    width,
                    "  ",
                    first_line_emitted,
                    format_first_reasoning_line,
                    format_reasoning_body_line,
                ));
            }
            Some(ActiveStream::Assistant(_)) => {
                self.finish_stream(width);
                self.active_stream = Some(ActiveStream::Reasoning(text.to_string(), false));
            }
            None => {
                self.active_stream = Some(ActiveStream::Reasoning(text.to_string(), false));
            }
        }
    }

    pub fn finish_stream(&mut self, width: u16) {
        match self.active_stream.take() {
            Some(ActiveStream::Assistant(text)) => {
                self.lines.extend(assistant_body_lines(&text, width));
                self.lines.extend(assistant_footer(width));
            }
            Some(ActiveStream::Reasoning(buffer, _first_line_emitted)) if !buffer.is_empty() => {
                self.lines.extend(reasoning_block(&buffer, width));
            }
            Some(ActiveStream::Reasoning(..)) => {}
            None => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActiveStream {
    Assistant(String),
    /// buffer, first_line_emitted
    Reasoning(String, bool),
}

pub fn render_history_lines_to_buffer(lines: &[Line<'static>], buffer: &mut Buffer) {
    for (y, line) in lines.iter().enumerate().take(buffer.area.height as usize) {
        let area = Rect {
            x: 0,
            y: y as u16,
            width: buffer.area.width,
            height: 1,
        };
        line.clone().render(area, buffer);
    }
}

pub fn format_history_line(line: &RenderedLine, width: u16) -> Vec<Line<'static>> {
    match line {
        RenderedLine::User(s) => wrap_multiline_text(&format!("> {s}"), width),
        RenderedLine::Assistant(s) => assistant_block(s, width),
        RenderedLine::Reasoning(s) => reasoning_block(s, width),
        RenderedLine::ToolCall { name, args_preview } => {
            wrap_multiline_text(&format!("⚙ {name}({args_preview})"), width)
        }
        RenderedLine::ToolResult { name, output, ok } => {
            let glyph = if *ok { "✓" } else { "✗" };
            wrap_multiline_text(&format!("{glyph} {name}: {output}"), width)
        }
        RenderedLine::System(s) => wrap_multiline_text(&format!("[system] {s}"), width),
    }
}

pub fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

fn wrap_multiline_text(text: &str, width: u16) -> Vec<Line<'static>> {
    let normalized = text.replace("\r\n", "\n");
    let mut out = Vec::new();
    for segment in normalized.split('\n') {
        out.extend(wrap_to_width(segment, width));
    }
    out
}

fn wrap_to_width(text: &str, width: u16) -> Vec<Line<'static>> {
    let w = width.max(1) as usize;
    if visible_width(text) <= w {
        return vec![Line::from(text.to_string())];
    }

    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width.saturating_add(ch_width) > w && !current.is_empty() {
            out.push(Line::from(std::mem::take(&mut current)));
            current_width = 0;
        }
        current.push(ch);
        current_width += ch_width;
    }

    if !current.is_empty() {
        out.push(Line::from(current));
    }
    if out.is_empty() {
        out.push(Line::from(String::new()));
    }
    out
}

fn assistant_block(text: &str, width: u16) -> Vec<Line<'static>> {
    let mut out = assistant_header(width);
    out.extend(assistant_body_lines(text, width));
    out.extend(assistant_footer(width));
    out
}

fn assistant_header(width: u16) -> Vec<Line<'static>> {
    let w = width.max(1) as usize;
    let title = " ⚕ Perry Hermes ✦ ";
    let top = fit_line_to_width(format!("╭─{title}╮"), w, "╮");
    vec![Line::from(top).bold().cyan()]
}

fn assistant_body_lines(text: &str, width: u16) -> Vec<Line<'static>> {
    let w = width.max(1) as usize;
    let body_indent = "  ";
    let inner_w = w.saturating_sub(visible_width(body_indent)).max(1);
    let mut out = Vec::new();
    let normalized = text.replace("\r\n", "\n");
    for segment in normalized.split('\n') {
        for wrapped in wrap_to_width(segment, inner_w as u16) {
            out.push(Line::from(vec![
                Span::raw(body_indent.to_string()),
                Span::raw(line_text(&wrapped)),
            ]));
        }
    }
    out
}

/// Drain complete lines from a streaming buffer. A line is complete when a
/// `\n` is found or when the buffer reaches the available inner width.
/// Incomplete trailing text stays in the buffer.
///
/// `first_line_emitted` is flipped to `true` when the very first line is
/// produced; `format_first` receives that line, `format_cont` receives all
/// subsequent lines.
fn drain_stream(
    buffer: &mut String,
    width: u16,
    indent: &str,
    first_line_emitted: &mut bool,
    format_first: fn(&str) -> Line<'static>,
    format_cont: fn(&str) -> Line<'static>,
) -> Vec<Line<'static>> {
    let w = width.max(1) as usize;
    let inner_w = w.saturating_sub(visible_width(indent)).max(1);
    let mut out = Vec::new();

    loop {
        if let Some(newline_idx) = buffer.find('\n') {
            let mut line = buffer[..newline_idx].to_string();
            buffer.drain(..=newline_idx);
            if line.ends_with('\r') {
                line.pop();
            }
            for wrapped in wrap_to_width(&line, inner_w as u16) {
                let pick = if !*first_line_emitted {
                    *first_line_emitted = true;
                    format_first
                } else {
                    format_cont
                };
                out.push(pick(&line_text(&wrapped)));
            }
            continue;
        }

        if visible_width(buffer.as_str()) >= inner_w {
            let (line, rest) = split_at_display_width_owned(buffer.as_str(), inner_w);
            let pick = if !*first_line_emitted {
                *first_line_emitted = true;
                format_first
            } else {
                format_cont
            };
            out.push(pick(&line));
            *buffer = rest;
            continue;
        }

        break;
    }

    out
}

fn split_at_display_width_owned(s: &str, max_width: usize) -> (String, String) {
    let mut width = 0usize;
    let mut end = 0usize;
    for (idx, ch) in s.char_indices() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width.saturating_add(ch_width) > max_width && end > 0 {
            break;
        }
        width += ch_width;
        end = idx + ch.len_utf8();
        if width >= max_width {
            break;
        }
    }
    if end == 0 {
        // At least one character must be emitted even if it overflows.
        if let Some(ch) = s.chars().next() {
            return (ch.to_string(), s[ch.len_utf8()..].to_string());
        }
    }
    (s[..end].to_string(), s[end..].to_string())
}

fn format_assistant_body_line(text: &str) -> Line<'static> {
    Line::from(vec![Span::raw("  "), Span::raw(text.to_string())])
}

fn format_reasoning_body_line(text: &str) -> Line<'static> {
    reasoning_line("  ", text)
}

fn format_first_reasoning_line(text: &str) -> Line<'static> {
    reasoning_line("✦ ", text)
}

fn reasoning_line(prefix: &str, text: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw(prefix.to_string()),
        Span::raw(text.to_string()),
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    )
}

fn assistant_footer(width: u16) -> Vec<Line<'static>> {
    let w = width.max(1) as usize;
    let bot = fit_line_to_width("╰─╯".to_string(), w, "╯");
    vec![Line::from(bot).cyan()]
}

fn reasoning_block(text: &str, width: u16) -> Vec<Line<'static>> {
    let normalized = text.replace("\r\n", "\n");
    let mut out = Vec::new();
    for (idx, segment) in normalized.split('\n').enumerate() {
        let prefix = if idx == 0 { "✦ " } else { "  " };
        for (line_idx, wrapped) in wrap_to_width(segment, width.saturating_sub(prefix.len() as u16))
            .into_iter()
            .enumerate()
        {
            let head = if line_idx == 0 { prefix } else { "  " };
            out.push(
                Line::from(vec![
                    Span::raw(head.to_string()),
                    Span::raw(line_text(&wrapped)),
                ])
                .style(
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ),
            );
        }
    }
    if out.is_empty() {
        out.push(
            Line::from("✦").style(
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
        );
    }
    out
}

fn fit_line_to_width(mut s: String, width: usize, suffix: &str) -> String {
    let suffix_width = visible_width(suffix);
    while visible_width(&s) < width {
        let insert_at = s.len().saturating_sub(suffix.len());
        s.insert(insert_at, '─');
    }
    while visible_width(&s) > width && visible_width(&s) > suffix_width {
        let insert_at = s.len().saturating_sub(suffix.len() + '─'.len_utf8());
        if insert_at >= s.len() {
            break;
        }
        s.remove(insert_at);
    }
    s
}

fn visible_width(text: &str) -> usize {
    text.width()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_buffers_lines_into_history() {
        let mut app = App::default();
        let mut history = HistoryWrite::default();

        assert!(history.lines.is_empty());
        history.push(&mut app, RenderedLine::User("hello".into()), 80);
        assert!(!history.lines.is_empty(), "push should buffer lines");

        history.clear();
        assert!(history.lines.is_empty(), "clear should empty the buffer");
    }
}
