//! Tests for the input layer: typing, backspace, and Enter -> Submit.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hermes_cli::tui::app::App;
use hermes_cli::tui::event::{AppEvent, RenderedLine};
use hermes_cli::tui::input::handle_key;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn typing_appends_to_input_buffer() {
    let mut app = App::new_for_test();
    let ev = handle_key(&mut app, key(KeyCode::Char('h')));
    assert!(matches!(ev, AppEvent::Tick));
    let ev = handle_key(&mut app, key(KeyCode::Char('i')));
    assert!(matches!(ev, AppEvent::Tick));
    assert_eq!(app.input, "hi");
}

#[test]
fn backspace_removes_last_char() {
    let mut app = App::new_for_test();
    app.input.push_str("hello");
    let ev = handle_key(&mut app, key(KeyCode::Backspace));
    assert!(matches!(ev, AppEvent::Tick));
    assert_eq!(app.input, "hell");
}

#[test]
fn enter_submits_input() {
    let mut app = App::new_for_test();
    app.input.push_str("hi there");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    assert!(matches!(ev, AppEvent::Submit(text) if text == "hi there"));
    assert_eq!(app.input, "");
}

#[test]
fn slash_quit_produces_quit_event() {
    let mut app = App::new_for_test();
    app.input.push_str("/quit");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    assert!(matches!(ev, AppEvent::Quit));
    assert_eq!(app.input, "");
}

#[test]
fn slash_exit_produces_quit_event() {
    let mut app = App::new_for_test();
    app.input.push_str("/exit");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    assert!(matches!(ev, AppEvent::Quit));
}

#[test]
fn slash_compact_with_focus_produces_compact_event() {
    let mut app = App::new_for_test();
    app.input.push_str("/compact focus on shell commands");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    assert!(matches!(ev, AppEvent::Compact(Some(focus)) if focus == "focus on shell commands"));
}

#[test]
fn slash_compact_without_focus_produces_compact_event() {
    let mut app = App::new_for_test();
    app.input.push_str("/compact");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    assert!(matches!(ev, AppEvent::Compact(None)));
}

#[test]
fn slash_clear_produces_clear_event() {
    let mut app = App::new_for_test();
    app.input.push_str("/clear");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    assert!(matches!(ev, AppEvent::Clear));
}

#[test]
fn unknown_slash_command_is_rejected_with_system_message() {
    let mut app = App::new_for_test();
    app.input.push_str("/bogus");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    match ev {
        AppEvent::Append(RenderedLine::System(s)) => {
            assert!(s.contains("Unknown"), "system message should contain 'Unknown': {s}");
        }
        other => panic!("expected Append(System); got {other:?}"),
    }
    assert_eq!(app.input, "");
}