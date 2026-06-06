//! Tests for the input layer: typing, backspace, and Enter -> Submit.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hermes_cli::tui::app::App;
use hermes_cli::tui::event::AppEvent;
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