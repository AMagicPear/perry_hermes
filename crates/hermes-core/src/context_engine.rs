//! Context compression engine trait and supporting types.
//!
//! When a conversation approaches the model's context limit, a
//! [`ContextEngine`] implementation rewrites the session history into a
//! shorter prompt plus an LLM-generated summary.

use async_trait::async_trait;

use crate::message::Message;
use crate::Usage;

/// Result of one compression pass.
#[derive(Debug, Clone)]
pub struct CompressionResult {
    pub messages: Vec<Message>,
    /// Usage reported by the summary LLM call. This lets the loop compute a
    /// post-compact context signal from real provider token counts.
    pub summary_usage: Usage,
}

/// Trait for context compression engines.
///
/// A single trait, no ABC factory, no plugin registry. Agent crates can
/// provide one built-in implementation and keep the loop decoupled from the
/// message-rewriting details.
///
/// Methods that mutate engine state take `&mut self`. Callers store the
/// engine behind `Arc<tokio::sync::Mutex<dyn ContextEngine>>` for
/// interior mutability in the async loop.
#[async_trait]
pub trait ContextEngine: Send + Sync {
    /// Whether the loop may attempt automatic compression after a provider
    /// response crosses the configured context threshold.
    ///
    /// This is policy/backoff only. Token threshold checks live in the agent
    /// loop because only the loop sees provider-reported [`Usage`].
    fn can_compress_automatically(&self) -> bool {
        true
    }

    /// Heavy entry point. Returns the new (shorter) message list and the
    /// summary call usage reported by the provider.
    ///
    /// `focus_topic` is `Some(_)` for `/compact <focus>`, `None` otherwise.
    async fn compress(
        &mut self,
        messages: Vec<Message>,
        focus_topic: Option<&str>,
        force: bool,
    ) -> Result<CompressionResult, CompressError>;

    /// Called when `/new` or `/reset` is invoked. Reset per-session state.
    fn on_session_reset(&mut self);
}

/// Errors that can occur during context compression.
#[derive(Debug, thiserror::Error)]
pub enum CompressError {
    /// LLM summary call failed after retries.
    /// Caller should treat this as a fatal error for the current turn.
    #[error("summary failed: {0}")]
    SummaryFailed(String),
    /// No messages eligible for compression (everything is protected).
    #[error("nothing to compress")]
    NothingToCompress,
}

/// Where compression was triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionTrigger {
    /// After an API response (post-turn check).
    PostTurn,
    /// User invoked `/compact [focus]`.
    Manual,
}

/// Why compression was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionSkipReason {
    /// Last two compressions saved < 10% of tokens.
    Ineffective,
    /// No messages eligible for compression (everything is protected).
    NothingToCompress,
    /// Compression is disabled in config.
    Disabled,
}
