use std::sync::Arc;

use async_trait::async_trait;

use crate::runner::GatewayRunner;

/// Trait for platform adapters (Telegram, Discord, etc.).
///
/// Each adapter normalizes platform-specific messages into [`GatewayEvent`]
/// and dispatches them through the [`GatewayRunner`]. Adapters own
/// presentation and platform connection; they must NOT own prompt history.
///
/// [`GatewayEvent`]: crate::event::GatewayEvent
#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    /// Human-readable platform identifier (e.g. "telegram", "discord").
    fn name(&self) -> &str;

    /// Start receiving messages. For each incoming message the adapter
    /// should call `gateway.handle_event(event)` and send the returned
    /// response text back through `send_message()`.
    ///
    /// This method blocks until the adapter is shut down.
    async fn run(&self, gateway: Arc<GatewayRunner>) -> anyhow::Result<()>;

    /// Send a text message to the specified chat.
    async fn send_message(&self, chat_id: &str, text: &str) -> anyhow::Result<()>;

    /// Show a typing indicator (best-effort, fire-and-forget).
    async fn send_typing(&self, chat_id: &str) -> anyhow::Result<()>;

    /// Gracefully disconnect from the platform.
    async fn disconnect(&self) -> anyhow::Result<()>;
}
