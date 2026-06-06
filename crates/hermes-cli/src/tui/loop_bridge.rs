//! Translate `LoopEvent`s from the agent into `AppEvent`s the TUI consumes.

use crate::tui::app::App;
use crate::tui::event::{AppEvent, AppMode, RenderedLine};
use hermes_agent::LoopEvent;

/// Apply a `LoopEvent` to the `App`, returning the `AppEvent` the main loop
/// should dispatch next.
pub fn apply_loop_event(app: &mut App, ev: LoopEvent) -> AppEvent {
    match ev {
        LoopEvent::ContentDelta(text) => {
            if let Some(RenderedLine::Assistant(existing)) = app.scrollback.last_mut() {
                existing.push_str(&text);
            } else {
                app.push_line(RenderedLine::Assistant(text));
            }
            AppEvent::Tick
        }
        LoopEvent::ReasoningDelta(text) => {
            if let Some(RenderedLine::Reasoning(existing)) = app.scrollback.last_mut() {
                existing.push_str(&text);
            } else {
                app.push_line(RenderedLine::Reasoning(text));
            }
            AppEvent::Tick
        }
        LoopEvent::ToolCallPartial(_td) => {
            // Streaming metadata; arguments are incomplete. Wait for
            // ToolCallStarted to push the final line.
            AppEvent::Tick
        }
        LoopEvent::ToolCallStarted { call, iteration: _ } => {
            // Iteration tracking can be added later if needed
            let args_preview = call.arguments.to_string();
            app.push_line(RenderedLine::ToolCall {
                name: call.name,
                args_preview,
            });
            AppEvent::Tick
        }
        LoopEvent::ToolCallFinished { call, result } => {
            let (output, ok) = match result {
                Ok(tool_output) => (tool_output.content, true),
                Err(e) => (e.to_string(), false),
            };
            app.push_line(RenderedLine::ToolResult {
                name: call.name,
                output,
                ok,
            });
            AppEvent::Tick
        }
        LoopEvent::AssistantMessage(_) => {
            // Assistant message signals end of assistant turn → transition to idle
            app.mode = AppMode::Idle;
            AppEvent::Tick
        }
        LoopEvent::IterationsExhausted => {
            app.mode = AppMode::Idle;
            AppEvent::Tick
        }
        LoopEvent::Cancelled => {
            app.mode = AppMode::Idle;
            AppEvent::Tick
        }
        LoopEvent::CompressionCompleted {
            tokens_before,
            tokens_after,
            duration,
            ..
        } => {
            app.compression_hint = Some(format!(
                "Compressed: {} → {} tokens in {}ms",
                tokens_before,
                tokens_after,
                duration.as_millis()
            ));
            AppEvent::Tick
        }
        LoopEvent::CompressionSkipped { reason: _ } => {
            // Could log or display but no strong signal for the user
            AppEvent::Tick
        }
        LoopEvent::CompressionFailed { error, .. } => {
            app.compression_hint = Some(format!("Compression failed: {}", error));
            AppEvent::Tick
        }
        LoopEvent::Thinking => {
            // Thinking event — no scrollback change, just trigger redraw
            AppEvent::Tick
        }
        LoopEvent::LengthLimit => {
            // Terminal event — could set a hint but keep it minimal
            AppEvent::Tick
        }
    }
}