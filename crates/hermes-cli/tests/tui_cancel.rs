//! Cancellation semantics:
//! - First Ctrl-C while `AwaitingModel` triggers `AppEvent::CancelInFlight`
//!   (which the main loop uses to call `cancel.cancel()`).
//! - Second Ctrl-C in any mode produces `AppEvent::Quit`.
//! - Ctrl-D in `Idle` mode produces `AppEvent::Quit`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use perry_hermes_cli::tui::app::App;
use perry_hermes_cli::tui::event::{AppEvent, AppMode};
use perry_hermes_cli::tui::input::handle_key;

fn ctrl_c() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
}
fn ctrl_d() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)
}
fn esc() -> KeyEvent {
    KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
}

#[test]
fn first_ctrl_c_in_awaiting_emits_cancel_in_flight() {
    let mut app = App::default();
    app.mode = AppMode::AwaitingModel;
    let ev = handle_key(&mut app, ctrl_c());
    assert!(matches!(ev, AppEvent::CancelInFlight));
}

#[test]
fn second_ctrl_c_in_any_mode_emits_quit() {
    let mut app = App::default();
    app.mode = AppMode::Cancelling;
    let ev = handle_key(&mut app, ctrl_c());
    assert!(matches!(ev, AppEvent::Quit));
}

#[test]
fn ctrl_d_in_idle_emits_quit() {
    let mut app = App::default();
    app.mode = AppMode::Idle;
    let ev = handle_key(&mut app, ctrl_d());
    assert!(matches!(ev, AppEvent::Quit));
}

#[test]
fn ctrl_c_in_idle_emits_quit() {
    let mut app = App::default();
    app.mode = AppMode::Idle;
    let ev = handle_key(&mut app, ctrl_c());
    assert!(matches!(ev, AppEvent::Quit));
}

#[test]
fn esc_in_awaiting_emits_cancel_in_flight() {
    let mut app = App::default();
    app.mode = AppMode::AwaitingModel;
    let ev = handle_key(&mut app, esc());
    assert!(matches!(ev, AppEvent::CancelInFlight));
}

#[test]
fn esc_in_cancelling_emits_quit() {
    let mut app = App::default();
    app.mode = AppMode::Cancelling;
    let ev = handle_key(&mut app, esc());
    assert!(matches!(ev, AppEvent::Quit));
}

#[test]
fn esc_in_idle_is_ignored() {
    let mut app = App::default();
    app.mode = AppMode::Idle;
    let ev = handle_key(&mut app, esc());
    assert!(matches!(ev, AppEvent::Tick));
}

#[test]
fn ctrl_d_in_awaiting_is_ignored() {
    let mut app = App::default();
    app.mode = AppMode::AwaitingModel;
    let ev = handle_key(&mut app, ctrl_d());
    assert!(matches!(ev, AppEvent::Tick));
}
