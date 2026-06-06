//! KeyEvent -> AppEvent mapping, plus input-buffer editing.

use crate::tui::app::App;
use crate::tui::event::{AppEvent, AppMode, RenderedLine};
use crossterm::event::{KeyCode, KeyEvent};

/// Page-down (or arrow-down) scrolls one viewport-height toward the bottom.
const SCROLL_PAGE: u16 = 10;

/// Apply a key event to the App's input buffer. Returns the AppEvent that
/// the main loop should process.
pub fn handle_key(app: &mut App, key: KeyEvent) -> AppEvent {
    use crossterm::event::KeyModifiers;
    // Ctrl-C: cancellation. First press while AwaitingModel -> CancelInFlight;
    // second press (in any mode) -> Quit.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return match app.mode {
            AppMode::AwaitingModel | AppMode::Idle => AppEvent::CancelInFlight,
            AppMode::Cancelling => AppEvent::Quit,
        };
    }
    // In Cancelling mode, ignore all other keys until the in-flight turn ends.
    if app.mode == AppMode::Cancelling {
        return AppEvent::Tick;
    }
    // Ctrl-D: only quits from Idle.
    if key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return match app.mode {
            AppMode::Idle => AppEvent::Quit,
            _ => AppEvent::Tick,
        };
    }
    // Chat scroll keys (only when not awaiting — while streaming we always
    // auto-scroll to bottom so the user can watch the reply).
    if app.mode == AppMode::Idle {
        match key.code {
            KeyCode::PageUp => {
                app.chat_scroll = app.chat_scroll.saturating_add(SCROLL_PAGE);
                return AppEvent::Tick;
            }
            KeyCode::PageDown => {
                app.chat_scroll = app.chat_scroll.saturating_sub(SCROLL_PAGE);
                return AppEvent::Tick;
            }
            KeyCode::End => {
                app.chat_scroll = 0;
                return AppEvent::Tick;
            }
            _ => {}
        }
    }
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
        "/compact" => AppEvent::Compact(if rest.is_empty() {
            None
        } else {
            Some(rest.to_string())
        }),
        "/clear" => AppEvent::Clear,
        other => AppEvent::Append(RenderedLine::System(format!(
            "Unknown command: {other}. Try /quit, /exit, /compact [focus], /clear."
        ))),
    }
}
