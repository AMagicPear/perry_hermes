//! Tests for the input layer: typing, backspace, and Enter -> Submit.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use perry_hermes_cli::tui::app::App;
use perry_hermes_cli::tui::event::{AppEvent, AppMode, RenderedLine};
use perry_hermes_cli::tui::input::handle_key;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn typing_appends_to_input_buffer() {
    let mut app = App::default();
    let ev = handle_key(&mut app, key(KeyCode::Char('h')));
    assert!(matches!(ev, AppEvent::Tick));
    let ev = handle_key(&mut app, key(KeyCode::Char('i')));
    assert!(matches!(ev, AppEvent::Tick));
    assert_eq!(app.input, "hi");
}

#[test]
fn backspace_removes_last_char() {
    let mut app = App::default();
    app.input.push_str("hello");
    app.cursor = 5;
    let ev = handle_key(&mut app, key(KeyCode::Backspace));
    assert!(matches!(ev, AppEvent::Tick));
    assert_eq!(app.input, "hell");
}

#[test]
fn enter_submits_input() {
    let mut app = App::default();
    app.input.push_str("hi there");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    assert!(matches!(ev, AppEvent::Submit(text) if text == "hi there"));
    assert_eq!(app.input, "");
}

#[test]
fn enter_in_awaiting_model_does_not_submit_parallel_turn() {
    let mut app = App::default();
    app.mode = AppMode::AwaitingModel;
    app.input.push_str("queued thought");
    app.cursor = app.input.len();

    let ev = handle_key(&mut app, key(KeyCode::Enter));

    assert!(matches!(ev, AppEvent::Tick));
    assert_eq!(app.input, "queued thought");
}

#[test]
fn slash_quit_produces_quit_event() {
    let mut app = App::default();
    app.input.push_str("/quit");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    assert!(matches!(ev, AppEvent::Quit));
    assert_eq!(app.input, "");
}

#[test]
fn slash_exit_produces_quit_event() {
    let mut app = App::default();
    app.input.push_str("/exit");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    assert!(matches!(ev, AppEvent::Quit));
}

#[test]
fn slash_compact_with_focus_produces_compact_event() {
    let mut app = App::default();
    app.input.push_str("/compact focus on shell commands");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    assert!(matches!(ev, AppEvent::Compact(Some(focus)) if focus == "focus on shell commands"));
}

#[test]
fn slash_compact_without_focus_produces_compact_event() {
    let mut app = App::default();
    app.input.push_str("/compact");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    assert!(matches!(ev, AppEvent::Compact(None)));
}

#[test]
fn slash_clear_produces_clear_event() {
    let mut app = App::default();
    app.input.push_str("/clear");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    assert!(matches!(ev, AppEvent::Clear));
}

#[test]
fn unknown_slash_command_is_rejected_with_system_message() {
    let mut app = App::default();
    app.input.push_str("/bogus");
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
}

#[test]
fn cancelling_mode_ignores_typing() {
    use perry_hermes_cli::tui::event::AppMode;

    let mut app = App::default();
    app.mode = AppMode::Cancelling;
    // Type a character — should be ignored.
    let ev = handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
    );
    assert!(
        matches!(ev, AppEvent::Tick),
        "expected Tick for ignored char in Cancelling; got {ev:?}"
    );
    assert!(app.input.is_empty(), "input must not grow in Cancelling");

    // Press Enter — should be ignored (no Submit).
    let ev = handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        matches!(ev, AppEvent::Tick),
        "expected Tick for ignored Enter in Cancelling; got {ev:?}"
    );
    assert!(app.input.is_empty(), "input must stay empty");

    // Backspace — should be ignored.
    let ev = handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
    );
    assert!(
        matches!(ev, AppEvent::Tick),
        "expected Tick for ignored Backspace in Cancelling; got {ev:?}"
    );
}

#[test]
fn arrow_up_scrolls_chat_when_idle() {
    use perry_hermes_cli::tui::event::AppMode;

    let mut app = App::default();
    app.mode = AppMode::Idle;
    let ev = handle_key(&mut app, key(KeyCode::Up));
    assert!(matches!(ev, AppEvent::Tick));
    assert_eq!(app.chat_scroll, 1);
}

#[test]
fn arrow_down_scrolls_chat_toward_bottom_when_idle() {
    use perry_hermes_cli::tui::event::AppMode;

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
