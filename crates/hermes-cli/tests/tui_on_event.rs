//! Verifies that `tui::make_on_event` forwards `LoopEvent`s into the
//! TUI's mpsc channel as `AppEvent::Loop`.

use hermes_agent::LoopEvent;
use hermes_cli::tui::event::AppEvent;
use hermes_cli::tui::make_on_event;
use tokio::sync::mpsc;

#[tokio::test]
async fn on_event_forwards_content_delta() {
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut on_event = make_on_event(tx);

    on_event(LoopEvent::ContentDelta("hi".to_string()));

    let received = rx.recv().await.expect("event");
    // Use matches! because AppEvent/LoopEvent do not derive PartialEq
    // (ToolError is thiserror-only and blocks a full PartialEq chain).
    assert!(matches!(received, AppEvent::Loop(LoopEvent::ContentDelta(v)) if v == "hi"));
}

#[tokio::test]
async fn on_event_forwards_reasoning_delta() {
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut on_event = make_on_event(tx);

    on_event(LoopEvent::ReasoningDelta("thinking...".to_string()));

    let received = rx.recv().await.expect("event");
    assert!(matches!(received, AppEvent::Loop(LoopEvent::ReasoningDelta(v)) if v == "thinking..."));
}

#[tokio::test]
async fn on_event_forwards_thinking() {
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut on_event = make_on_event(tx);

    on_event(LoopEvent::Thinking);

    let received = rx.recv().await.expect("event");
    assert!(matches!(received, AppEvent::Loop(LoopEvent::Thinking)));
}
