//! KeyEvent -> AppEvent mapping, plus input-buffer editing.

use crate::tui::app::App;
use crate::tui::event::{AppEvent, AppMode, RenderedLine};
use crossterm::event::{KeyCode, KeyEvent};
use perry_hermes_core::commands::Command;

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
            AppMode::AwaitingModel => AppEvent::CancelInFlight,
            AppMode::Idle => AppEvent::Quit,
            AppMode::Cancelling => AppEvent::Quit,
        };
    }
    if key.code == KeyCode::Esc {
        return match app.mode {
            AppMode::AwaitingModel => AppEvent::CancelInFlight,
            AppMode::Idle => AppEvent::Tick,
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
    // Chat scroll keys (only when idle).
    if app.mode == AppMode::Idle {
        match key.code {
            KeyCode::Up => {
                app.chat_scroll = app.chat_scroll.saturating_add(1);
                return AppEvent::Tick;
            }
            KeyCode::Down => {
                app.chat_scroll = app.chat_scroll.saturating_sub(1);
                return AppEvent::Tick;
            }
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
            app.insert_at_cursor(c);
            AppEvent::Tick
        }
        KeyCode::Backspace => {
            app.delete_before_cursor();
            AppEvent::Tick
        }
        KeyCode::Delete => {
            app.delete_at_cursor();
            AppEvent::Tick
        }
        KeyCode::Left => {
            app.move_cursor_left();
            AppEvent::Tick
        }
        KeyCode::Right => {
            app.move_cursor_right();
            AppEvent::Tick
        }
        KeyCode::Home => {
            app.move_cursor_home();
            AppEvent::Tick
        }
        KeyCode::End => {
            // End in Idle mode was handled above for chat scroll, but if
            // we reach here, it means we're in AwaitingModel. Move cursor to
            // end of input.
            app.move_cursor_end();
            AppEvent::Tick
        }
        KeyCode::Enter => {
            if app.mode == AppMode::Cancelling {
                return AppEvent::Tick;
            }
            let text = std::mem::take(&mut app.input);
            app.cursor = 0;
            if text.trim().is_empty() {
                return AppEvent::Tick;
            }
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

    match Command::parse(trimmed) {
        Some(parsed) => match parsed.command {
            Command::Quit => AppEvent::Quit,
            Command::Compact => AppEvent::Compact(parsed.arg),
            Command::Clear => AppEvent::Clear,
            // Gateway-only commands are not valid in the TUI
            other => AppEvent::Append(RenderedLine::System(format!(
                "Unknown command: /{}. Try /quit, /exit, /compact [focus], /clear.",
                other.meta().name,
            ))),
        },
        None => {
            // Not a known command — check if it looks like a command at all
            let word = trimmed.split_whitespace().next().unwrap_or("");
            AppEvent::Append(RenderedLine::System(format!(
                "Unknown command: {word}. Try /quit, /exit, /compact [focus], /clear."
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::App;
    use crate::tui::event::{AppEvent, RenderedLine};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    #[test]
    fn typing_appends_at_cursor() {
        let mut app = App::default();
        app.input = "helo".to_string();
        app.cursor = 3;
        let ev = handle_key(&mut app, key(KeyCode::Char('l')));
        assert!(matches!(ev, AppEvent::Tick));
        assert_eq!(app.input, "hello");
        assert_eq!(app.cursor, 4);
    }

    #[test]
    fn backspace_deletes_before_cursor() {
        let mut app = App::default();
        app.input = "hello".to_string();
        app.cursor = 5;
        let ev = handle_key(&mut app, key(KeyCode::Backspace));
        assert!(matches!(ev, AppEvent::Tick));
        assert_eq!(app.input, "hell");
        assert_eq!(app.cursor, 4);
    }

    #[test]
    fn backspace_respects_cursor_position() {
        let mut app = App::default();
        app.input = "abcd".to_string(); // cursor position 2
        app.cursor = 2;
        handle_key(&mut app, key(KeyCode::Backspace));
        assert_eq!(app.input, "acd");
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn delete_removes_char_at_cursor() {
        let mut app = App::default();
        app.input = "abcd".to_string();
        app.cursor = 1;
        let ev = handle_key(&mut app, key(KeyCode::Delete));
        assert!(matches!(ev, AppEvent::Tick));
        assert_eq!(app.input, "acd");
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn delete_noop_at_end() {
        let mut app = App::default();
        app.input = "hi".to_string();
        app.cursor = 2;
        handle_key(&mut app, key(KeyCode::Delete));
        assert_eq!(app.input, "hi");
    }

    #[test]
    fn left_arrow_moves_cursor_backward() {
        let mut app = App::default();
        app.input = "hello".to_string();
        app.cursor = 5;
        let ev = handle_key(&mut app, key(KeyCode::Left));
        assert!(matches!(ev, AppEvent::Tick));
        assert_eq!(app.cursor, 4);
    }

    #[test]
    fn right_arrow_moves_cursor_forward() {
        let mut app = App::default();
        app.input = "hello".to_string();
        app.cursor = 0;
        let ev = handle_key(&mut app, key(KeyCode::Right));
        assert!(matches!(ev, AppEvent::Tick));
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn left_arrow_stays_at_start() {
        let mut app = App::default();
        app.input = "hi".to_string();
        let ev = handle_key(&mut app, key(KeyCode::Left));
        assert!(matches!(ev, AppEvent::Tick));
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn right_arrow_stays_at_end() {
        let mut app = App::default();
        app.input = "hi".to_string();
        app.cursor = 2;
        let ev = handle_key(&mut app, key(KeyCode::Right));
        assert!(matches!(ev, AppEvent::Tick));
        assert_eq!(app.cursor, 2);
    }

    #[test]
    fn home_moves_cursor_to_start() {
        let mut app = App::default();
        app.input = "hello".to_string();
        app.cursor = 3;
        let ev = handle_key(&mut app, key(KeyCode::Home));
        assert!(matches!(ev, AppEvent::Tick));
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn end_moves_cursor_to_end() {
        let mut app = App::default();
        app.mode = AppMode::AwaitingModel; // so End is not intercepted by chat scroll
        app.input = "hello".to_string();
        app.cursor = 0;
        let ev = handle_key(&mut app, key(KeyCode::End));
        assert!(matches!(ev, AppEvent::Tick));
        assert_eq!(app.cursor, 5);
    }

    #[test]
    fn enter_submits_input() {
        let mut app = App::default();
        app.input = "hi there".to_string();
        app.cursor = 2;
        let ev = handle_key(&mut app, key(KeyCode::Enter));
        assert!(matches!(ev, AppEvent::Submit(text) if text == "hi there"));
        assert_eq!(app.input, "");
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn enter_in_awaiting_model_submits_to_queue() {
        let mut app = App::default();
        app.mode = AppMode::AwaitingModel;
        app.input = "queued thought".to_string();
        app.cursor = app.input.len();

        let ev = handle_key(&mut app, key(KeyCode::Enter));

        assert!(matches!(ev, AppEvent::Submit(text) if text == "queued thought"));
        assert_eq!(app.input, "");
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn slash_quit_produces_quit_event() {
        let mut app = App::default();
        app.input = "/quit".to_string();
        app.cursor = 5;
        let ev = handle_key(&mut app, key(KeyCode::Enter));
        assert!(matches!(ev, AppEvent::Quit));
        assert_eq!(app.input, "");
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn slash_exit_produces_quit_event() {
        let mut app = App::default();
        app.input = "/exit".to_string();
        let ev = handle_key(&mut app, key(KeyCode::Enter));
        assert!(matches!(ev, AppEvent::Quit));
    }

    #[test]
    fn slash_compact_with_focus_produces_compact_event() {
        let mut app = App::default();
        app.input = "/compact focus on shell commands".to_string();
        let ev = handle_key(&mut app, key(KeyCode::Enter));
        assert!(matches!(ev, AppEvent::Compact(Some(focus)) if focus == "focus on shell commands"));
    }

    #[test]
    fn slash_compact_without_focus_produces_compact_event() {
        let mut app = App::default();
        app.input = "/compact".to_string();
        let ev = handle_key(&mut app, key(KeyCode::Enter));
        assert!(matches!(ev, AppEvent::Compact(None)));
    }

    #[test]
    fn slash_clear_produces_clear_event() {
        let mut app = App::default();
        app.input = "/clear".to_string();
        let ev = handle_key(&mut app, key(KeyCode::Enter));
        assert!(matches!(ev, AppEvent::Clear));
    }

    #[test]
    fn unknown_slash_command_is_rejected_with_system_message() {
        let mut app = App::default();
        app.input = "/bogus".to_string();
        app.cursor = 6;
        let ev = handle_key(&mut app, key(KeyCode::Enter));
        match ev {
            AppEvent::Append(RenderedLine::System(s)) => {
                assert!(
                    s.contains("Unknown"),
                    "system message should mention 'Unknown': {s}"
                );
                assert!(
                    s.contains("/quit") && s.contains("/compact") && s.contains("/exit"),
                    "system message should list all known commands: {s}"
                );
            }
            other => panic!("expected Append(System); got {other:?}"),
        }
        assert_eq!(app.input, "");
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn cancelling_mode_ignores_typing() {
        use crate::tui::event::AppMode;

        let mut app = App::default();
        app.mode = AppMode::Cancelling;
        // Type a character — should be ignored.
        let ev = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), crossterm::event::KeyModifiers::NONE),
        );
        assert!(
            matches!(ev, AppEvent::Tick),
            "expected Tick for ignored char in Cancelling; got {ev:?}"
        );
        assert!(app.input.is_empty(), "input must not grow in Cancelling");

        // Press Enter — should be ignored (no Submit).
        let ev = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::NONE),
        );
        assert!(
            matches!(ev, AppEvent::Tick),
            "expected Tick for ignored Enter in Cancelling; got {ev:?}"
        );
        assert!(app.input.is_empty(), "input must stay empty");

        // Backspace — should be ignored.
        let ev = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Backspace, crossterm::event::KeyModifiers::NONE),
        );
        assert!(
            matches!(ev, AppEvent::Tick),
            "expected Tick for ignored Backspace in Cancelling; got {ev:?}"
        );
    }

    #[test]
    fn arrow_up_scrolls_chat_when_idle() {
        let mut app = App::default();
        app.mode = AppMode::Idle;
        let ev = handle_key(&mut app, key(KeyCode::Up));
        assert!(matches!(ev, AppEvent::Tick));
        assert_eq!(app.chat_scroll, 1);
    }

    #[test]
    fn arrow_down_scrolls_chat_toward_bottom_when_idle() {
        let mut app = App::default();
        app.mode = AppMode::Idle;
        app.chat_scroll = 3;
        let ev = handle_key(&mut app, key(KeyCode::Down));
        assert!(matches!(ev, AppEvent::Tick));
        assert_eq!(app.chat_scroll, 2);
    }

    #[test]
    fn clear_event_resets_scrollback_and_cache() {
        let mut app = App::default();
        app.push_line(RenderedLine::Assistant("hello".to_string()));
        assert!(!app.chat_lines_for_width(20).is_empty());

        app.clear_scrollback();
        assert!(app.scrollback.is_empty());
        assert!(app.chat_lines_for_width(20).is_empty());
        assert_eq!(app.chat_scroll, 0);
    }

    #[test]
    fn left_arrow_in_awaiting_model_moves_cursor() {
        let mut app = App::default();
        app.mode = AppMode::AwaitingModel;
        app.input = "test".to_string();
        app.cursor = 4;
        // Left should move cursor, not scroll chat
        handle_key(&mut app, key(KeyCode::Left));
        assert_eq!(app.cursor, 3);
        assert_eq!(app.chat_scroll, 0);
    }

    #[test]
    fn cjk_cursor_movement_by_character() {
        let mut app = App::default();
        app.input = "你好世界".to_string();
        app.cursor = 12; // end
        // Move left past "界" (3 bytes)
        handle_key(&mut app, key(KeyCode::Left));
        assert_eq!(app.cursor, 9); // before "界"
        // Move left past "世" (3 bytes)
        handle_key(&mut app, key(KeyCode::Left));
        assert_eq!(app.cursor, 6); // before "世界"
        // Insert at cursor
        handle_key(&mut app, key(KeyCode::Char(',')));
        assert_eq!(app.input, "你好,世界");
        assert_eq!(app.cursor, 7);
    }
}
