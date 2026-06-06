//! KeyEvent -> AppEvent mapping, plus input-buffer editing.

use crate::tui::app::App;
use crate::tui::event::AppEvent;
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
            AppEvent::Submit(text)
        }
        _ => AppEvent::Tick,
    }
}