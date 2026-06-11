//! `ratatui`-based TUI. Replaces the legacy REPL.

pub mod app;
pub mod event;
pub mod history;
pub mod input;
pub mod loop_bridge;
pub mod render;
pub mod run;

use perry_hermes_agent::LoopEvent;
use perry_hermes_gateway::{GatewayEventHandler, dispatch_loop_event};
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
struct TuiEventHandler {
    tx: mpsc::UnboundedSender<AppEvent>,
}

impl TuiEventHandler {
    fn new(tx: mpsc::UnboundedSender<AppEvent>) -> Self {
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

    fn on_tool_started(&mut self, call: &perry_hermes_core::message::ToolCall, iteration: u32) {
        let _ = self.tx.send(AppEvent::Loop(LoopEvent::ToolCallStarted {
            call: call.clone(),
            iteration,
        }));
    }

    fn on_tool_finished(
        &mut self,
        call: &perry_hermes_core::message::ToolCall,
        result: &Result<perry_hermes_core::tool::ToolOutput, perry_hermes_core::error::ToolError>,
    ) {
        let _ = self.tx.send(AppEvent::Loop(LoopEvent::ToolCallFinished {
            call: call.clone(),
            result: result.clone(),
        }));
    }

    fn on_assistant_message(&mut self, message: &perry_hermes_core::message::Message) {
        let _ = self
            .tx
            .send(AppEvent::Loop(LoopEvent::AssistantMessage(message.clone())));
    }
}

/// Build the `on_event` closure to pass to `AgentLoop::run_session_turn`.
///
/// Each `LoopEvent` is dispatched through a [`TuiEventHandler`]
/// (implementing [`GatewayEventHandler`]) and forwarded into the TUI's
/// main loop as an `AppEvent::Loop`. The TUI uses the same streaming
/// protocol as all other platform adapters.
pub fn make_on_event(tx: mpsc::UnboundedSender<AppEvent>) -> impl FnMut(LoopEvent) + Send {
    let mut handler = TuiEventHandler::new(tx);
    move |ev: LoopEvent| {
        dispatch_loop_event(&mut handler, &ev);
    }
}
