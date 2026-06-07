//! The `Provider` trait — the only LLM-facing abstraction in the codebase.

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::error::ProviderError;
use crate::message::Message;
use crate::registry::ToolSchema;
use crate::usage::Usage;

/// Something that can answer a `complete()` call. Every LLM backend
/// (OpenAI, Anthropic, an echo mock, …) implements this.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Open a streaming completion for the given messages + tools.
    ///
    /// The returned stream yields `CompletionDelta`s; consumers either
    /// drive it manually (see `AgentLoop`) or call `complete()` for the
    /// default accumulation path.
    ///
    /// Cancellation: the consumer MUST `select!` on `cancel` and drop
    /// the stream when cancelled. Dropping the stream aborts the
    /// in-flight HTTP body.
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<CompletionStream, ProviderError>;

    /// Drive the stream to a single `Completion`. Default impl uses
    /// `accumulate_stream`. Providers do not need to override unless they
    /// can do this more efficiently than the SSE parse + accumulate path
    /// (none do today).
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<Completion, ProviderError> {
        let stream = self.stream(messages, tools, cancel).await?;
        crate::accumulator::accumulate_stream(stream).await
    }
}

/// A single LLM response, post-parse.
#[derive(Debug, Clone)]
pub struct Completion {
    pub message: Message,
    pub usage: Usage,
    pub finish_reason: FinishReason,
}

/// Why the LLM stopped generating. Mapped from each provider's
/// `finish_reason` string at the adapter layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    /// Normal completion (`stop`).
    Stop,
    /// LLM wants to call one or more tools (`tool_calls` / `tool_use`).
    ToolUse,
    /// Hit the model's `max_tokens` (`length`).
    Length,
    /// Provider blocked the response (`content_filter`).
    ContentFilter,
    /// Provider's own error (`error`).
    Error,
}

impl FinishReason {
    /// Parse a provider's `finish_reason` string into our enum.
    /// Unknown values map to `Error` so callers can decide what to do
    /// rather than silently treating unknown as Stop.
    pub fn from_provider_str(s: &str) -> Self {
        match s {
            "stop" => FinishReason::Stop,
            "tool_calls" | "tool_use" => FinishReason::ToolUse,
            "length" => FinishReason::Length,
            "content_filter" => FinishReason::ContentFilter,
            "error" => FinishReason::Error,
            _ => FinishReason::Error,
        }
    }
}

/// A stream of incremental deltas. Implementations should yield each
/// `CompletionDelta` as soon as the corresponding chunk arrives.
///
/// The stream yields `Result<CompletionDelta, ProviderError>` so callers can
/// propagate provider-level errors via `?`.
pub type CompletionStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<CompletionDelta, ProviderError>> + Send>>;

/// One chunk of a streaming tool call. OpenAI emits these incrementally:
/// the first chunk for a given `index` carries `id` and `name`; later
/// chunks carry `arguments_fragment`.
///
/// `arguments_fragment` is a **partial** JSON string, not a complete value.
/// `StreamAccumulator` concatenates fragments per-`index` and re-parses
/// the joined result back into a structured `Value` at the end of the
/// stream. Consumers should never `serde_json::from_str` a single
/// fragment; it almost certainly isn't valid JSON on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments_fragment: Option<String>,
}

/// One chunk of a streaming response.
#[derive(Debug, Clone)]
pub struct CompletionDelta {
    pub content_delta: Option<String>,
    pub reasoning_delta: Option<String>,
    pub tool_call_delta: Option<ToolCallDelta>,
    pub usage: Option<Usage>,
    pub finish_reason: Option<FinishReason>,
}

// Re-exported so provider callers have one stable import path while the
// implementation stays in `crate::accumulator`.
pub use crate::accumulator::{accumulate_stream, StreamAccumulator};
