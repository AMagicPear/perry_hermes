//! KeyEvent -> AppEvent mapping, plus input-buffer editing.

use crate::tui::app::App;
use crate::tui::event::{AppEvent, RenderedLine};
use crossterm::event::{KeyCode, KeyEvent};

/// Apply a key event to the App's input buffer. Returns the AppEvent that
/// the main loop should process.
pub fn handle_key(app: &mut App, key: KeyEvent) -> AppEvent {
    match key.code {
        KeyCode::Char(c) => {
            app.input.push(c);
            AppEvent::Tick
        }
        KeyCode::Backspace => {
            app.input.pop();
            AppEvent::Tick
        }
        KeyCode::Enter => {
            let text = std::mem::take(&mut app.input);
            parse_slash_or_submit(text)
        }
        _ => AppEvent::Tick,
    }
}

fn parse_slash_or_submit(text: String) -> AppEvent {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return AppEvent::Submit(text);
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();

    match cmd {
        "/quit" | "/exit" => AppEvent::Quit,
        "/compact" => AppEvent::Compact(if rest.is_empty() { None } else { Some(rest.to_string()) }),
        "/clear" => AppEvent::Clear,
        other => AppEvent::Append(RenderedLine::System(format!(
            "Unknown command: {other}. Try /quit, /exit, /compact [focus], /clear."
        ))),
    }
}