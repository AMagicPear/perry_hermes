//! Tests for translating `LoopEvent` -> `AppEvent`.

use perry_hermes_agent::LoopEvent;
use perry_hermes_cli::tui::app::App;
use perry_hermes_cli::tui::event::{AppEvent, AppMode, RenderedLine};
use perry_hermes_cli::tui::loop_bridge::apply_loop_event;
use perry_hermes_core::compaction_strategy::{CompressionSkipReason, CompressionTrigger};
use perry_hermes_core::error::ToolError;
use perry_hermes_core::message::ToolCall;
use perry_hermes_core::provider::ToolCallDelta;
use perry_hermes_core::tool::ToolOutput;
use std::time::Duration;

fn app_with_mode(mode: AppMode) -> App {
    let mut app = App::default();
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
fn content_delta_invalidates_cached_wrapping() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    app.push_line(RenderedLine::Assistant("hello".to_string()));
    let initial_lines = app.chat_lines_for_width(12).len();

    let _ = apply_loop_event(&mut app, LoopEvent::ContentDelta(" world".to_string()));
    let updated_lines = app.chat_lines_for_width(12).len();

    assert!(updated_lines >= initial_lines);
    match app.scrollback.last() {
        Some(RenderedLine::Assistant(text)) => assert_eq!(text, "hello world"),
        other => panic!("expected assistant line after delta; got {other:?}"),
    }
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
fn tool_call_partial_does_not_push_scrollback_line() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    // ToolCallPartial is a streaming event with incomplete arguments;
    // it should not push any scrollback line.
    let ev = LoopEvent::ToolCallPartial(ToolCallDelta {
        index: 0,
        id: Some("call_abc".to_string()),
        name: Some("terminal".to_string()),
        arguments_fragment: Some("{\"cmd\":\"ls\"}".to_string()),
    });
    let _ = apply_loop_event(&mut app, ev);
    assert!(
        app.scrollback.is_empty(),
        "ToolCallPartial should not push a scrollback line; got {:?}",
        app.scrollback
    );
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
        result: Ok(ToolOutput {
            content: "file1\nfile2".to_string(),
        }),
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
    let ev = LoopEvent::AssistantMessage(perry_hermes_core::message::Message {
        role: perry_hermes_core::message::Role::Assistant,
        content: perry_hermes_core::message::Content::Text("done".to_string()),
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
        context_tokens: None,
        compacted_tokens: Some(12_345),
        duration: Duration::from_millis(1_200),
    };
    let _ = apply_loop_event(&mut app, ev);
    assert_eq!(
        app.compression_hint.as_deref(),
        Some("Compressed in 1200ms")
    );
    assert_eq!(app.context_used_tokens, Some(12_345));
}

#[test]
fn compression_skipped_sets_hint() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    let ev = LoopEvent::CompressionSkipped {
        reason: CompressionSkipReason::NothingToCompress,
    };

    let _ = apply_loop_event(&mut app, ev);

    assert_eq!(app.compression_hint.as_deref(), Some("Nothing to compact"));
}

#[test]
fn context_usage_event_updates_status_value() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    let ev = LoopEvent::ContextUsageUpdated {
        used_tokens: 42_000,
    };

    let next = apply_loop_event(&mut app, ev);

    assert!(matches!(next, AppEvent::Tick));
    assert_eq!(app.context_used_tokens, Some(42_000));
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

#[test]
fn tool_call_finished_error_includes_error_message() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    let ev = LoopEvent::ToolCallFinished {
        call: ToolCall {
            id: "call_xyz".to_string(),
            name: "terminal".to_string(),
            arguments: serde_json::json!({}),
        },
        result: Err(ToolError::Execution("command not found: foo".to_string())),
    };
    let _ = apply_loop_event(&mut app, ev);
    match app.scrollback.last() {
        Some(RenderedLine::ToolResult {
            name,
            output,
            ok: false,
        }) => {
            assert_eq!(name, "terminal");
            assert!(
                output.contains("command not found"),
                "error output should include the actual error message; got: {output:?}"
            );
        }
        other => panic!("expected ToolResult with ok=false; got {other:?}"),
    }
}

#[test]
fn read_file_tool_result_is_summarized_for_tui() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    let ev = LoopEvent::ToolCallFinished {
        call: ToolCall {
            id: "call_read".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({}),
        },
        result: Ok(ToolOutput {
            content: serde_json::json!({
                "content": "1|# Title\n2|line one\n3|line two\n4|line three\n5|line four\n6|line five\n",
                "total_lines": 2000,
                "file_size": 123456,
                "truncated": true,
                "_hint": "Use offset=7 to continue reading (showing 1-6 of 2000 lines)"
            })
            .to_string(),
        }),
    };
    let _ = apply_loop_event(&mut app, ev);
    match app.scrollback.last() {
        Some(RenderedLine::ToolResult { name, output, ok }) => {
            assert_eq!(name, "read_file");
            assert!(*ok);
            assert!(
                output.contains("# Title") && output.contains("line one"),
                "expected preview content in summarized tool output: {output:?}"
            );
            assert!(
                output.contains("Use offset=7"),
                "expected pagination hint in summarized tool output: {output:?}"
            );
            assert!(
                !output.contains("\"file_size\""),
                "raw JSON should not be rendered into TUI: {output:?}"
            );
        }
        other => panic!("expected ToolResult preview; got {other:?}"),
    }
}
