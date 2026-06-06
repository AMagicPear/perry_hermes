//! Frame painter for the TUI.

use crate::tui::app::App;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// Paint one frame.
pub fn render(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // chat area
            Constraint::Length(3), // input box
            Constraint::Length(1), // status bar
        ])
        .split(f.area());

    // Chat area: a placeholder paragraph listing the lines we have so far.
    let chat_lines: Vec<Line> = app
        .scrollback
        .iter()
        .map(|line| match line {
            crate::tui::event::RenderedLine::User(s) => Line::from(format!("> {s}")),
            crate::tui::event::RenderedLine::Assistant(s) => Line::from(s.clone()),
            crate::tui::event::RenderedLine::Reasoning(s) => Line::from(format!("… {s}")),
            crate::tui::event::RenderedLine::ToolCall { name, args_preview } => {
                Line::from(format!("⚙ {name}({args_preview})"))
            }
            crate::tui::event::RenderedLine::ToolResult { name, output, ok } => {
                Line::from(format!("{} {name}: {}", if *ok { "✓" } else { "✗" }, output))
            }
            crate::tui::event::RenderedLine::System(s) => Line::from(format!("[system] {s}")),
        })
        .collect();
    let chat = Paragraph::new(chat_lines).block(Block::default().borders(Borders::NONE));
    f.render_widget(chat, chunks[0]);

    // Input box.
    let input_text = if app.input.is_empty() {
        "Type a message and press Enter. /quit, /compact [focus], /clear.".to_string()
    } else {
        app.input.clone()
    };
    let input = Paragraph::new(input_text)
        .block(Block::default().borders(Borders::ALL).title("Input"));
    f.render_widget(input, chunks[1]);

    // Status bar.
    let status = Paragraph::new(Line::from("● idle")).style(Style::default());
    f.render_widget(status, chunks[2]);
}