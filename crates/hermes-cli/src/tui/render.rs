//! Frame painter for the TUI.

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::tui::app::App;
use crate::tui::event::{AppMode, RenderedLine};
use crate::tui::shimmer::shimmer_spans;

/// A 5-row ASCII art banner spelling "HERMES" (figlet "small" style).
/// Painted exactly once at the start of the TUI.
const WELCOME_BANNER: &[&str] = &[
    " _   _ _____ ___  __  __ _____ ____  ",
    "| | | | ____/ _ \\|  \\/  | ____|  _ \\ ",
    "| |_| |  _|| | | | |\\/| |  _| | |_) |",
    "|  _  | |__| |_| | |  | | |___|  _ < ",
    "|_| |_|_____\\___/|_|  |_|_____|_| \\_\\",
];

const TIP_LINE: &str = "✦ Tip: press / to see available commands.";

/// Paint one frame.
pub fn render(f: &mut Frame, app: &App) {
    let welcome_h = if app.welcome_shown { 0 } else { WELCOME_BANNER.len() as u16 };
    let tip_h = if app.welcome_shown { 0 } else { 1 };
    let working_h = if matches!(app.mode, AppMode::AwaitingModel) { 1 } else { 0 };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(welcome_h), // welcome banner
            Constraint::Length(tip_h),      // tip line
            Constraint::Min(1),            // chat scrollback
            Constraint::Length(working_h), // working indicator (awaiting only)
            Constraint::Length(1),         // status row 1
            Constraint::Length(3),         // input block
        ])
        .split(f.area());

    // --- Welcome banner (one-shot) -----------------------------------------
    if !app.welcome_shown {
        let mut banner_lines: Vec<Line> = WELCOME_BANNER
            .iter()
            .map(|row| Line::from(shimmer_spans(row)))
            .collect();
        banner_lines.push(Line::from(TIP_LINE).dim());
        let banner = Paragraph::new(banner_lines)
            .block(Block::default().borders(Borders::NONE))
            .alignment(ratatui::layout::Alignment::Center);
        f.render_widget(banner, chunks[0]);
    }

    // --- Chat scrollback ----------------------------------------------------
    let chat_area = chunks[2];
    let chat_lines = build_chat_lines(&app.scrollback, chat_area.width);
    let chat = Paragraph::new(chat_lines)
        .block(Block::default().borders(Borders::NONE))
        .wrap(Wrap { trim: false });
    f.render_widget(chat, chat_area);

    // --- Working indicator (only when awaiting) -----------------------------
    if matches!(app.mode, AppMode::AwaitingModel) {
        let working = build_working_line(app);
        let working_widget = Paragraph::new(working)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::NONE));
        f.render_widget(working_widget, chunks[3]);
    }

    // --- Status row 1 (always visible) --------------------------------------
    let status_line = build_status_line_1(app);
    let status = Paragraph::new(status_line)
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::NONE));
    f.render_widget(status, chunks[4]);

    // --- Input block --------------------------------------------------------
    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
    let input_text = build_input_line(app);
    let input = Paragraph::new(input_text)
        .block(input_block)
        .wrap(Wrap { trim: false });
    f.render_widget(input, chunks[5]);
}

/// Build the chat-area `Vec<Line>` from the scrollback, wrapping assistant
/// content in a rounded `╭─ Hermes ─...─╮` block.
fn build_chat_lines(scrollback: &[RenderedLine], width: u16) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    for line in scrollback {
        match line {
            RenderedLine::User(s) => out.push(Line::from(format!("> {s}"))),
            RenderedLine::Assistant(s) => {
                out.extend(assistant_block(s, width));
            }
            RenderedLine::Reasoning(s) => {
                out.push(Line::from(format!("… {s}")).dim());
            }
            RenderedLine::ToolCall { name, args_preview } => {
                out.push(Line::from(format!("⚙ {name}({args_preview})")));
            }
            RenderedLine::ToolResult { name, output, ok } => {
                let glyph = if *ok { "✓" } else { "✗" };
                out.push(Line::from(format!("{glyph} {name}: {output}")));
            }
            RenderedLine::System(s) => {
                out.push(Line::from(format!("[system] {s}")));
            }
        }
    }
    out
}

/// Render assistant text inside a rounded box of `width` columns.
/// Top: `╭─ Hermes ─...─╮`, body: `│ <wrapped text> │`, bottom: `╰─...─╯`.
fn assistant_block(text: &str, width: u16) -> Vec<Line<'static>> {
    let w = width.max(20) as usize;
    // Inner content width = total width - 4 (for `│ ` and ` │`).
    let inner_w = w.saturating_sub(4).max(1);
    let title = " Hermes ";
    // Top border: `╭─ Hermes ─...─╮` — title takes (2 + title.len() + 1) cols,
    // fill the rest with `─`.
    let top_prefix = "╭─";
    let top_suffix = "╮";
    let top_filler_dashes =
        w.saturating_sub(top_prefix.len() + title.len() + top_suffix.len());
    let top = format!("{top_prefix}{title}{}", "─".repeat(top_filler_dashes));
    // Bottom border: `╰─...─╯`
    let bot_prefix = "╰─";
    let bot_suffix = "╯";
    let bot_filler_dashes = w.saturating_sub(bot_prefix.len() + bot_suffix.len());
    let bot = format!("{bot_prefix}{}{bot_suffix}", "─".repeat(bot_filler_dashes));

    let mut out: Vec<Line<'static>> = Vec::new();
    out.push(Line::from(top).bold().cyan());

    // Wrap text into lines of `inner_w` columns. Hard-wrap on character
    // boundaries (preserves spaces, breaks words wider than inner_w).
    let mut current = String::new();
    for word in text.split_whitespace() {
        // Truncate an overlong word to inner_w.
        let word = if word.chars().count() > inner_w {
            word.chars().take(inner_w).collect::<String>()
        } else {
            word.to_string()
        };
        if current.is_empty() {
            current.push_str(&word);
        } else if current.chars().count() + 1 + word.chars().count() <= inner_w {
            current.push(' ');
            current.push_str(&word);
        } else {
            out.push(Line::from(format!(
                "│ {:<inner_w$} │",
                current
            )));
            current = word;
        }
    }
    if !current.is_empty() {
        out.push(Line::from(format!("│ {:<inner_w$} │", current)));
    } else if text.is_empty() {
        out.push(Line::from(format!("│ {:<inner_w$} │", "")));
    }

    out.push(Line::from(bot));
    out
}

/// Build status row 1: `⚕ {provider} · {model} · {in_tok} / {total} {pct}% · {elapsed} · iter {i}/{max} · {mode}`.
/// The `{in_tok} / {total} {pct}%` segment is omitted when
/// `app.context_window_size` is `None`.
fn build_status_line_1(app: &App) -> Line<'static> {
    let provider = app.provider_name.as_deref().unwrap_or("?");
    let model = app.model_name.as_deref().unwrap_or("?");
    let mode = match app.mode {
        AppMode::Idle => "idle",
        AppMode::AwaitingModel => "awaiting",
        AppMode::Cancelling => "cancelling",
    };
    let elapsed = app
        .turn_started_at
        .map(|t| fmt_elapsed_compact(t.elapsed().as_secs()))
        .unwrap_or_else(|| "—".to_string());
    let iter_str = format!("iter {}/{}", app.iteration, app.max_iterations);

    let mut spans: Vec<Span<'static>> = vec![
        Span::raw("⚕ "),
        Span::raw(provider.to_string()),
        Span::raw(" · "),
        Span::raw(model.to_string()),
    ];

    if let Some(total) = app.context_window_size {
        let in_tok = app.last_input_tokens.unwrap_or(0);
        let pct = ((in_tok as f64 / total as f64) * 100.0).clamp(0.0, 100.0) as u64;
        spans.push(Span::raw(" · "));
        spans.push(Span::raw(format!(
            "{} / {}",
            format_tokens(in_tok),
            format_tokens(total)
        )));
        spans.push(Span::raw(format!(" {pct}%")));
        spans.push(Span::raw(" · "));
        spans.push(Span::raw(elapsed));
    } else {
        spans.push(Span::raw(" · "));
        spans.push(Span::raw(elapsed));
    }

    spans.push(Span::raw(" · "));
    spans.push(Span::raw(iter_str));
    spans.push(Span::raw(" · "));
    spans.push(Span::raw(mode.to_string()));
    Line::from(spans)
}

/// Build the working-indicator line: `⠋ Working · {elapsed} · {N} tool call(s) · esc to interrupt`.
fn build_working_line(app: &App) -> Line<'static> {
    let elapsed = app
        .turn_started_at
        .map(|t| fmt_elapsed_compact(t.elapsed().as_secs()))
        .unwrap_or_else(|| "0s".to_string());
    let tool_str = if app.iteration == 1 {
        "1 tool call".to_string()
    } else {
        format!("{} tool calls", app.iteration)
    };
    Line::from(vec![
        Span::raw("⠋ "),
        Span::raw("Working").bold(),
        Span::raw(" · "),
        Span::raw(elapsed),
        Span::raw(" · "),
        Span::raw(tool_str),
        Span::raw(" · esc to interrupt").dim(),
    ])
}

/// Build the input line: `❯ {text_or_placeholder}`.
fn build_input_line(app: &App) -> Line<'static> {
    let prompt = Span::styled(
        "❯ ",
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
        app.last_input_tokens = Some(200_000);
        let line = build_status_line_1(&app);
        let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(s.contains("20%"), "expected 20% in status; got {s:?}");
    }

    #[test]
    fn assistant_block_has_rounded_corners() {
        let lines = assistant_block("hello", 40);
        let s: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(s.contains('╭'), "expected top-left border; got {s:?}");
        assert!(s.contains('╰'), "expected bottom-left border; got {s:?}");
        assert!(s.contains("Hermes"), "expected title; got {s:?}");
    }

    #[test]
    fn input_line_has_arrow_prompt() {
        let app = App::new_for_test();
        let line = build_input_line(&app);
        let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(s.contains('❯'), "expected ❯ prompt; got {s:?}");
    }

    #[test]
    fn working_line_built_when_awaiting() {
        let mut app = App::new_for_test();
        app.mode = AppMode::AwaitingModel;
        app.turn_started_at = Some(std::time::Instant::now() - Duration::from_secs(5));
        app.iteration = 3;
        let line = build_working_line(&app);
        let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(s.contains("Working"), "expected 'Working' label; got {s:?}");
        assert!(
            s.contains("3 tool calls"),
            "expected iteration count; got {s:?}"
        );
        assert!(
            s.contains("esc to interrupt"),
            "expected interrupt hint; got {s:?}"
        );
    }
}
