//! TUI platform adapter.
//!
//! Bridges the terminal UI with the [`GatewayRunner`] so the TUI
//! participates in the same session management and turn serialization
//! as Telegram, QQ, and other messaging platforms.
//!
//! [`GatewayRunner`]: perry_hermes_gateway::GatewayRunner

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use perry_hermes_core::Platform;
use perry_hermes_gateway::event::{ChatType, GatewayEvent};
use perry_hermes_gateway::{GatewayRunner, PlatformAdapter};

/// Terminal UI adapter implementing [`PlatformAdapter`].
///
/// Each user submission becomes a [`GatewayEvent`] dispatched through
/// the shared [`GatewayRunner`]. Streaming agent output is delivered
/// back through a [`TuiEventHandler`] that forwards events to the
/// TUI's main loop via an mpsc channel.
///
/// [`TuiEventHandler`]: super::TuiEventHandler
pub struct TuiAdapter;

impl TuiAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl PlatformAdapter for TuiAdapter {
    fn name(&self) -> &str {
        "tui"
    }

    /// The TUI's `run` function drives the terminal event loop
    /// externally; this method is not called for the TUI adapter.
    async fn run(&self, _gateway: Arc<GatewayRunner>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn disconnect(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Build a [`GatewayEvent`] from a user submission.
pub fn make_gateway_event(text: String) -> GatewayEvent {
    GatewayEvent {
        platform: Platform::Tui,
        chat_id: std::process::id().to_string(),
        chat_type: ChatType::Dm,
        user_id: "local".into(),
        user_name: None,
        thread_id: None,
        text,
        message_id: None,
        timestamp: Utc::now(),
    }
}

/// Build the session key that [`GatewayRunner`] will use for TUI events.
/// This must match the key produced by `build_key(event)` — and
/// `Platform::Tui.as_str()` is `"cli"` (not `"tui"`) for backwards
/// compatibility with on-disk session files, so the key has to be
/// `cli:dm:{pid}`.
pub fn tui_session_key() -> String {
    format!("cli:dm:{}", std::process::id())
}
