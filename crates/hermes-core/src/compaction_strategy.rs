//! Context compaction strategy trait and supporting types.
//!
//! When a conversation approaches the model's context limit, a
//! [`CompactionStrategy`] implementation rewrites the session history into a
//! shorter prompt plus an LLM-generated summary.

use async_trait::async_trait;

use crate::message::Message;
use crate::Usage;

/// Result of one compression pass.
#[derive(Debug, Clone)]
pub struct CompactionResult {
    pub messages: Vec<Message>,
    /// Usage reported by the summary LLM call. This lets the loop compute a
    /// post-compact context signal from real provider token counts.
    pub summary_usage: Usage,
}

/// Trait for context compaction strategies.
///
/// This is not a session object. It owns only the policy for turning a list
/// of messages into a shorter list. Session lifetime, token facts, history,
/// cancellation, and reset behavior belong to `AgentSession`.
///
/// Methods that mutate engine state take `&mut self`. Callers store the
/// strategy behind `Arc<tokio::sync::Mutex<dyn CompactionStrategy>>` for
/// interior mutability in the async loop.
#[async_trait]
pub trait CompactionStrategy: Send + Sync {
    /// Heavy entry point. Returns the new (shorter) message list and the
    /// summary call usage reported by the provider.
    ///
    /// `focus_topic` is `Some(_)` for `/compact <focus>`, `None` otherwise.
    async fn compact(
        &mut self,
        messages: Vec<Message>,
        focus_topic: Option<&str>,
    ) -> Result<CompactionResult, CompactError>;
}

/// Errors that can occur during context compression.
#[derive(Debug, thiserror::Error)]
pub enum CompactError {
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
    /// No messages eligible for compression (everything is protected).
    NothingToCompress,
    /// Compression is disabled in config.
    Disabled,
}
