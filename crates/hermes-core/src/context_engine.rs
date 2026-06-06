//! Context compression engine trait and supporting types.
//!
//! When a conversation approaches the model's context limit, a
//! [`ContextEngine`] implementation compresses the middle turns via an
//! LLM-generated summary while preserving the head (system prompt +
//! earliest messages) and the tail (most recent messages).

use async_trait::async_trait;

use crate::message::Message;

/// Trait for context compression engines.
///
/// A single trait, no ABC factory, no plugin registry. The default
/// implementation is [`ContextCompressor`](crate::context::ContextCompressor).
///
/// Methods that mutate engine state take `&mut self`. Callers store the
/// engine behind `Arc<tokio::sync::Mutex<dyn ContextEngine>>` for
/// interior mutability in the async loop.
#[async_trait]
pub trait ContextEngine: Send + Sync {
    /// Cheap pre-call check. Default impl returns `false` (no preflight).
    /// The default `ContextCompressor` overrides this with a rough token estimate.
    fn should_compress(&self) -> bool {
        false
    }

    /// Heavy entry point. Returns the new (possibly shorter) message list.
    ///
    /// Implementations must preserve:
    ///   - system prompt (always)
    ///   - first `protect_first_n` non-system messages (head)
    ///   - last `protect_last_n` messages / last 20K tokens of user messages (tail)
    ///
    /// `focus_topic` is `Some(_)` for `/compress <focus>`, `None` otherwise.
    /// `current_tokens` is the rough pre-call estimate when known.
    async fn compress(
        &mut self,
        messages: Vec<Message>,
        current_tokens: Option<u64>,
        focus_topic: Option<&str>,
        force: bool,
    ) -> Result<Vec<Message>, CompressError>;

    /// Called when `/new` or `/reset` is invoked. Reset per-session state.
    fn on_session_reset(&mut self);

    /// Called when the model or context length changes.
    fn update_model(&mut self, model: &str, context_length: u64);

    /// Get the configured threshold in tokens.
    fn threshold_tokens(&self) -> u64;
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
    /// Before the next API call (pre-turn check).
    PreTurn,
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
