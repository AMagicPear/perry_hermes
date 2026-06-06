//! Tests for translating `LoopEvent` -> `AppEvent`.

use hermes_agent::LoopEvent;
use hermes_cli::tui::app::App;
use hermes_cli::tui::event::{AppEvent, AppMode, RenderedLine};
use hermes_cli::tui::loop_bridge::apply_loop_event;
use hermes_core::context_engine::CompressionTrigger;
use hermes_core::message::ToolCall;
use hermes_core::provider::ToolCallDelta;
use hermes_core::tool::ToolOutput;
use std::time::Duration;

fn app_with_mode(mode: AppMode) -> App {
    let mut app = App::new_for_test();
    app.mode = mode;
    app
}

#[test]
fn content_delta_appends_assistant_text() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    let ev = LoopEvent::ContentDelta("hello ".to_string());
    let next = apply_loop_event(&mut app, ev);
    assert!(matches!(next, AppEvent::Tick));
    assert_eq!(
        app.scrollback.last(),
        Some(&RenderedLine::Assistant("hello ".to_string()))
    );
}

#[test]
fn reasoning_delta_appends_reasoning_text() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    let ev = LoopEvent::ReasoningDelta("thinking...".to_string());
    let next = apply_loop_event(&mut app, ev);
    assert!(matches!(next, AppEvent::Tick));
    assert_eq!(
        app.scrollback.last(),
        Some(&RenderedLine::Reasoning("thinking...".to_string()))
    );
}

#[test]
fn tool_call_partial_pushes_tool_call_line() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    // ToolCallPartial carries id, name, and arguments_delta incrementally
    let ev = LoopEvent::ToolCallPartial(ToolCallDelta {
        index: 0,
        id: Some("call_abc".to_string()),
        name: Some("terminal".to_string()),
        arguments_delta: Some("{\"cmd\":\"ls\"}".to_string()),
    });
    let _ = apply_loop_event(&mut app, ev);
    assert!(matches!(
        app.scrollback.last(),
        Some(RenderedLine::ToolCall { name, args_preview }) if name == "terminal"
    ));
}

#[test]
fn tool_call_finished_pushes_tool_result_line() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    let ev = LoopEvent::ToolCallFinished {
        call: ToolCall {
            id: "call_abc".to_string(),
            name: "terminal".to_string(),
            arguments: serde_json::json!({}),
        },
        result: Ok(ToolOutput { content: "file1\nfile2".to_string() }),
    };
    let _ = apply_loop_event(&mut app, ev);
    assert!(matches!(
        app.scrollback.last(),
        Some(RenderedLine::ToolResult { name, ok: true, .. }) if name == "terminal"
    ));
}

#[test]
fn assistant_message_transitions_to_idle() {
    // AssistantMessage signals end of assistant turn - transition to idle
    let mut app = app_with_mode(AppMode::AwaitingModel);
    let ev = LoopEvent::AssistantMessage(hermes_core::message::Message {
        role: hermes_core::message::Role::Assistant,
        content: hermes_core::message::Content::Text("done".to_string()),
        reasoning: None,
        tool_calls: None,
        tool_call_id: None,
    });
    let next = apply_loop_event(&mut app, ev);
    assert!(matches!(next, AppEvent::Tick));
    assert_eq!(app.mode, AppMode::Idle);
}

#[test]
fn compression_completed_sets_hint() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    let ev = LoopEvent::CompressionCompleted {
        trigger: CompressionTrigger::Manual,
        tokens_before: 142_000,
        tokens_after: 38_000,
        summary_chars: 200,
        duration: Duration::from_millis(1_200),
    };
    let _ = apply_loop_event(&mut app, ev);
    assert!(app.compression_hint.is_some());
}

#[test]
fn iterations_exhausted_transitions_to_idle() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    let ev = LoopEvent::IterationsExhausted;
    let next = apply_loop_event(&mut app, ev);
    assert!(matches!(next, AppEvent::Tick));
    assert_eq!(app.mode, AppMode::Idle);
}

#[test]
fn cancelled_transitions_to_idle() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    let ev = LoopEvent::Cancelled;
    let next = apply_loop_event(&mut app, ev);
    assert!(matches!(next, AppEvent::Tick));
    assert_eq!(app.mode, AppMode::Idle);
}