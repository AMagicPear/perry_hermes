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
    /// response text back to the user through the platform's own API.
    ///
    /// This method blocks until the adapter is shut down.
    async fn run(&self, gateway: Arc<GatewayRunner>) -> anyhow::Result<()>;

    /// Gracefully disconnect from the platform.
    async fn disconnect(&self) -> anyhow::Result<()>;
}
