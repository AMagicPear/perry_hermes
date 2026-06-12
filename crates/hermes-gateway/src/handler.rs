//! Unified streaming event handler protocol.
//!
//! [`GatewayEventHandler`] is the core trait that all platform adapters
//! implement to receive agent output in real time. The gateway calls
//! trait methods as the agent loop produces events — each content
//! segment, tool invocation, and turn boundary is delivered through
//! this interface.
//!
//! # Design
//!
//! The trait mirrors what the TUI already does internally (see
//! `hermes-cli::tui::loop_bridge`): accumulate streaming text, flush
//! at iteration boundaries (tool calls), and signal turn completion.
//! Messaging platforms (QQ, Telegram) use the same protocol to send
//! each content segment as a separate message.
//!
//! # Lifecycle
//!
//! For a typical multi-iteration agent turn:
//!
//! ```text
//! on_thinking()
//! on_content_delta("text from iteration 1...")
//! on_tool_started(call)          ← flush accumulated content
//! on_tool_finished(call, result)
//! on_content_delta("text from iteration 2...")
//! on_turn_completed()            ← flush remaining content
//! ```

use perry_hermes_agent::LoopEvent;
use perry_hermes_core::error::ToolError;
use perry_hermes_core::message::{Message, ToolCall};
use perry_hermes_core::tool::ToolOutput;

/// Trait for receiving streaming agent events.
///
/// All methods have default no-op implementations so adapters only
/// need to override the events they care about.
///
/// Methods are synchronous — adapters that need async I/O (like sending
/// platform messages) should buffer content in `on_content_delta` and
/// flush at boundaries (`on_tool_started`, `on_turn_completed`).
pub trait GatewayEventHandler: Send {
    /// The agent is about to call the provider (start of a new iteration).
    fn on_thinking(&mut self) {}

    /// A text chunk from the LLM. Arrives incrementally as the provider
    /// streams tokens. Adapters should buffer this and flush at the next
    /// boundary.
    fn on_content_delta(&mut self, _text: &str) {}

    /// A reasoning/thinking chunk from the LLM (extended thinking models).
    fn on_reasoning_delta(&mut self, _text: &str) {}

    /// The agent is about to execute a tool call. This is a natural flush
    /// boundary — any accumulated content should be sent to the user now.
    fn on_tool_started(&mut self, _call: &ToolCall, _iteration: u32) {}

    /// A tool call finished executing.
    fn on_tool_finished(&mut self, _call: &ToolCall, _result: &Result<ToolOutput, ToolError>) {}

    /// An error occurred during the agent turn. Adapters should notify
    /// the user.
    fn on_error(&mut self, _error: &str) {}

    /// An assistant message was completed (end of one LLM iteration).
    /// This fires after each iteration's content has been fully streamed,
    /// and before any tool calls from that iteration.
    fn on_assistant_message(&mut self, _message: &Message) {}

    /// The agent turn completed successfully. Any remaining accumulated
    /// content should be flushed now.
    fn on_turn_completed(&mut self) {}

    /// Context usage was updated after a provider response.
    /// Used by the TUI to display the context window status bar.
    fn on_context_usage_updated(&mut self, _used_tokens: u64) {}

    /// Automatic compression completed. The TUI uses this to update
    /// the context usage display and show a compression hint.
    fn on_compression_completed(
        &mut self,
        _context_tokens: Option<u64>,
        _compacted_tokens: Option<u64>,
        _duration: std::time::Duration,
    ) {
    }

    /// A queued user message was just drained from the session's
    /// pending queue and is now part of the active turn. Adapters
    /// should display it as a normal user message at this point —
    /// the message is no longer transient, the agent has it.
    fn on_user_message_injected(&mut self, _text: &str) {}
}

/// A no-op handler used for events that don't need streaming (e.g.
/// slash command responses).
pub struct NoopHandler;

impl GatewayEventHandler for NoopHandler {}

/// Dispatch a raw [`LoopEvent`] from the agent loop to the appropriate
/// [`GatewayEventHandler`] method.
///
/// This bridges the agent's `FnMut(LoopEvent)` callback interface with
/// the handler trait. Platform adapters that receive raw `LoopEvent`s
/// (e.g. from `AgentLoop::run_session_turn`) can use this to route
/// events through their `GatewayEventHandler` implementation.
pub fn dispatch_loop_event(handler: &mut dyn GatewayEventHandler, event: &LoopEvent) {
    match event {
        LoopEvent::Thinking => handler.on_thinking(),
        LoopEvent::ContentDelta(text) => handler.on_content_delta(text),
        LoopEvent::ReasoningDelta(text) => handler.on_reasoning_delta(text),
        LoopEvent::ToolCallStarted { call, iteration } => {
            handler.on_tool_started(call, *iteration);
        }
        LoopEvent::ToolCallFinished { call, result } => {
            handler.on_tool_finished(call, result);
        }
        LoopEvent::AssistantMessage(msg) => {
            handler.on_assistant_message(msg);
        }
        LoopEvent::ContextUsageUpdated { used_tokens } => {
            handler.on_context_usage_updated(*used_tokens);
        }
        LoopEvent::CompressionCompleted {
            context_tokens,
            compacted_tokens,
            duration,
            ..
        } => {
            handler.on_compression_completed(*context_tokens, *compacted_tokens, *duration);
        }
        LoopEvent::UserMessageInjected(text) => {
            handler.on_user_message_injected(text);
        }
        // ToolCallPartial, LengthLimit, IterationsExhausted,
        // Cancelled, CompressionSkipped, CompressionFailed —
        // no handler dispatch needed.
        _ => {}
    }
}
