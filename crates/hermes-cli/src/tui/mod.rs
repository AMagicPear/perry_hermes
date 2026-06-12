//! `ratatui`-based TUI. Replaces the legacy REPL.

pub mod adapter;
pub mod app;
pub mod event;
pub mod history;
pub mod input;
pub mod loop_bridge;
pub mod render;
pub mod run;

use perry_hermes_agent::LoopEvent;
use perry_hermes_core::error::ToolError;
use perry_hermes_core::message::{Message, ToolCall};
use perry_hermes_core::tool::ToolOutput;
use perry_hermes_gateway::GatewayEventHandler;
use tokio::sync::mpsc;

pub use app::App;
pub use event::{AppEvent, AppMode, RenderedLine};
pub use run::{run, run_with_backend};

/// TUI implementation of [`GatewayEventHandler`].
///
/// Translates handler trait calls into `AppEvent::Loop(LoopEvent)`s
/// sent through the TUI's main loop channel. This is the TUI's
/// platform adapter — it uses the same streaming protocol as QQ and
/// Telegram, but delivers events to the ratatui rendering pipeline
/// instead of a messaging API.
pub struct TuiEventHandler {
    tx: mpsc::UnboundedSender<AppEvent>,
}

impl TuiEventHandler {
    pub fn new(tx: mpsc::UnboundedSender<AppEvent>) -> Self {
        Self { tx }
    }
}

impl GatewayEventHandler for TuiEventHandler {
    fn on_thinking(&mut self) {
        let _ = self.tx.send(AppEvent::Loop(LoopEvent::Thinking));
    }

    fn on_content_delta(&mut self, text: &str) {
        let _ = self
            .tx
            .send(AppEvent::Loop(LoopEvent::ContentDelta(text.to_string())));
    }

    fn on_reasoning_delta(&mut self, text: &str) {
        let _ = self
            .tx
            .send(AppEvent::Loop(LoopEvent::ReasoningDelta(text.to_string())));
    }

    fn on_tool_started(&mut self, call: &ToolCall, iteration: u32) {
        let _ = self.tx.send(AppEvent::Loop(LoopEvent::ToolCallStarted {
            call: call.clone(),
            iteration,
        }));
    }

    fn on_tool_finished(&mut self, call: &ToolCall, result: &Result<ToolOutput, ToolError>) {
        let _ = self.tx.send(AppEvent::Loop(LoopEvent::ToolCallFinished {
            call: call.clone(),
            result: result.clone(),
        }));
    }

    fn on_assistant_message(&mut self, message: &Message) {
        let _ = self
            .tx
            .send(AppEvent::Loop(LoopEvent::AssistantMessage(message.clone())));
    }

    fn on_error(&mut self, error: &str) {
        let _ = self
            .tx
            .send(AppEvent::Loop(LoopEvent::Cancelled)); // Reuse Cancelled for errors
        // Error text will be displayed via TurnCompleted error handling
        let _ = error; // Suppress unused warning
    }

    fn on_turn_completed(&mut self) {
        // TurnCompleted is sent via the result channel, not here.
    }
}

/// Build a `TuiEventHandler` for use with `GatewayRunner::handle_event`.
pub fn make_on_event(tx: mpsc::UnboundedSender<AppEvent>) -> TuiEventHandler {
    TuiEventHandler::new(tx)
}
