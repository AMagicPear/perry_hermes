//! `ratatui`-based TUI. Replaces the legacy REPL.

pub mod app;
pub mod event;
pub mod input;
pub mod loop_bridge;
pub mod render;
pub mod run;

use perry_hermes_agent::LoopEvent;
use tokio::sync::mpsc;

pub use app::App;
pub use event::{AppEvent, AppMode, RenderedLine};
pub use run::{run, run_with_backend};

/// Build the `on_event` closure to pass to `AIAgent::run_messages`. Each
/// `LoopEvent` is forwarded into the TUI's main loop as an `AppEvent::Loop`.
pub fn make_on_event(tx: mpsc::UnboundedSender<AppEvent>) -> impl FnMut(LoopEvent) + Send {
    move |ev: LoopEvent| {
        let _ = tx.send(AppEvent::Loop(ev));
    }
}
