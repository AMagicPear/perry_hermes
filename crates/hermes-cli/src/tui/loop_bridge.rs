//! Translate `LoopEvent`s from the agent into `AppEvent`s the TUI consumes.

use crate::tui::app::App;
use crate::tui::event::{AppEvent, AppMode, RenderedLine};
use perry_hermes_agent::LoopEvent;
use perry_hermes_core::compaction_strategy::CompressionSkipReason;
use serde_json::Value;

/// Apply a `LoopEvent` to the `App`, returning the `AppEvent` the main loop
/// should dispatch next.
pub fn apply_loop_event(app: &mut App, ev: LoopEvent) -> AppEvent {
    match ev {
        LoopEvent::ContentDelta(text) => {
            if let Some(RenderedLine::Assistant(existing)) = app.scrollback.last_mut() {
                existing.push_str(&text);
                app.mark_scrollback_dirty();
            } else {
                app.push_line(RenderedLine::Assistant(text));
            }
            AppEvent::Tick
        }
        LoopEvent::ReasoningDelta(text) => {
            if let Some(RenderedLine::Reasoning(existing)) = app.scrollback.last_mut() {
                existing.push_str(&text);
                app.mark_scrollback_dirty();
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
                Ok(tool_output) => (
                    summarize_tool_output(&call.name, &tool_output.content),
                    true,
                ),
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
        LoopEvent::ContextUsageUpdated { used_tokens } => {
            app.context_used_tokens = Some(used_tokens);
            AppEvent::Tick
        }
        LoopEvent::CompressionCompleted {
            context_tokens,
            compacted_tokens,
            duration,
            ..
        } => {
            let trigger = context_tokens
                .map(|tokens| format!(" at {} tokens", format_tokens(tokens)))
                .unwrap_or_default();
            if let Some(tokens) = compacted_tokens {
                app.context_used_tokens = Some(tokens);
            }
            app.compression_hint =
                Some(format!("Compressed{trigger} in {}ms", duration.as_millis()));
            AppEvent::Tick
        }
        LoopEvent::CompressionSkipped { reason } => {
            app.compression_hint = Some(compression_skip_hint(reason).to_string());
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
        LoopEvent::UserMessageInjected(text) => {
            // A queued user message has been drained from the session's
            // pending queue and is now part of the active turn. Render
            // it as a real user line in the scrollback; the status-bar
            // "queued: ..." preview will clear on the next tick.
            app.push_line(RenderedLine::User(text));
            AppEvent::Tick
        }
    }
}

fn compression_skip_hint(reason: CompressionSkipReason) -> &'static str {
    match reason {
        CompressionSkipReason::NothingToCompress => "Nothing to compact",
        CompressionSkipReason::Disabled => "Compression is disabled",
    }
}

fn summarize_tool_output(tool_name: &str, raw: &str) -> String {
    if tool_name == "read_file" {
        return summarize_read_file_output(raw);
    }
    summarize_generic_output(raw)
}

fn summarize_read_file_output(raw: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return raw.to_string();
    };
    let Some(content) = value.get("content").and_then(|v| v.as_str()) else {
        return raw.to_string();
    };
    let hint = value.get("_hint").and_then(|v| v.as_str()).unwrap_or("");
    let total_lines = value.get("total_lines").and_then(|v| v.as_i64());
    let truncated = value
        .get("truncated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut preview_lines: Vec<&str> = content.lines().take(12).collect();
    if preview_lines.is_empty() {
        preview_lines.push("(empty)");
    }

    let mut out = preview_lines.join("\n");
    if truncated || content.lines().count() > preview_lines.len() {
        out.push_str("\n…");
    }
    if let Some(total_lines) = total_lines {
        out.push_str(&format!("\n[{total_lines} lines total]"));
    }
    if !hint.is_empty() {
        out.push('\n');
        out.push_str(hint);
    }
    out
}

fn summarize_generic_output(raw: &str) -> String {
    const MAX_PREVIEW_LINES: usize = 20;
    const MAX_PREVIEW_CHARS: usize = 4_000;

    let mut preview = raw
        .lines()
        .take(MAX_PREVIEW_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    if preview.chars().count() > MAX_PREVIEW_CHARS {
        preview = preview.chars().take(MAX_PREVIEW_CHARS).collect();
        preview.push('…');
        return preview;
    }
    if raw.lines().count() > MAX_PREVIEW_LINES {
        preview.push_str("\n…");
    }
    preview
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::App;
    use crate::tui::event::RenderedLine;

    #[test]
    fn user_message_injected_pushes_user_line_to_scrollback() {
        let mut app = App::default();
        let scrollback_before = app.scrollback.len();
        assert!(matches!(app.mode, AppMode::Idle));

        let next = apply_loop_event(&mut app, LoopEvent::UserMessageInjected("可以了".into()));

        // The event translates into a Tick; the scrollback mutation
        // is the visible side effect.
        assert!(matches!(next, AppEvent::Tick));
        assert_eq!(app.scrollback.len(), scrollback_before + 1);
        match app.scrollback.last().expect("new line") {
            RenderedLine::User(text) => assert_eq!(text, "可以了"),
            other => panic!("expected User line; got {other:?}"),
        }
    }
}
