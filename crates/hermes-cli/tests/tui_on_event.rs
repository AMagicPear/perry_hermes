//! Verifies that `tui::make_on_event` forwards `LoopEvent`s into the
//! TUI's mpsc channel as `AppEvent::Loop`.

use perry_hermes_agent::LoopEvent;
use perry_hermes_cli::tui::event::AppEvent;
use perry_hermes_cli::tui::make_on_event;
use perry_hermes_gateway::GatewayEventHandler;
use tokio::sync::mpsc;

#[tokio::test]
async fn on_event_forwards_content_delta() {
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut on_event = make_on_event(tx);

    on_event.on_content_delta("hi");

    let received = rx.recv().await.expect("event");
    // Use matches! because AppEvent/LoopEvent do not derive PartialEq
    // (ToolError is thiserror-only and blocks a full PartialEq chain).
    assert!(matches!(received, AppEvent::Loop(LoopEvent::ContentDelta(v)) if v == "hi"));
}

#[tokio::test]
async fn on_event_forwards_reasoning_delta() {
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut on_event = make_on_event(tx);

    on_event.on_reasoning_delta("thinking...");

    let received = rx.recv().await.expect("event");
    assert!(matches!(received, AppEvent::Loop(LoopEvent::ReasoningDelta(v)) if v == "thinking..."));
}

#[tokio::test]
async fn on_event_forwards_thinking() {
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut on_event = make_on_event(tx);

    on_event.on_thinking();

    let received = rx.recv().await.expect("event");
    assert!(matches!(received, AppEvent::Loop(LoopEvent::Thinking)));
}
