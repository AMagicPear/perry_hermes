//! The `Provider` trait — the only LLM-facing abstraction in the codebase.

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::error::ProviderError;
use crate::message::{Message, ToolCall};
use crate::registry::ToolSchema;
use crate::usage::Usage;

/// Something that can answer a `complete()` call. Every LLM backend
/// (OpenAI, Anthropic, an echo mock, …) implements this.
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn model(&self) -> &str;

    /// Send messages, get a single completion back.
    ///
    /// `tools` is a list of JSON Schema objects describing available tools.
    /// `cancel` is a token the loop uses to say "stop, the user hit Ctrl-C".
    /// Implementations MUST select on `cancel` and bail out cleanly.
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<Completion, ProviderError>;

    /// Streaming variant. Default impl can fall back to `complete()` and
    /// yield a single chunk — providers that support streaming override it.
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<CompletionStream, ProviderError> {
        let _ = (messages, tools, &cancel);
        Err(ProviderError::Other("streaming not implemented".into()))
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
    /// Unknown values default to `Stop`.
    pub fn from_provider_str(s: &str) -> Self {
        match s {
            "stop" => FinishReason::Stop,
            "tool_calls" | "tool_use" => FinishReason::ToolUse,
            "length" => FinishReason::Length,
            "content_filter" => FinishReason::ContentFilter,
            "error" => FinishReason::Error,
            _ => FinishReason::Stop,
        }
    }
}

/// A stream of incremental deltas. Implementations should yield each
/// `CompletionDelta` as soon as the corresponding chunk arrives.
pub type CompletionStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = CompletionDelta> + Send>>;

/// One chunk of a streaming response.
#[derive(Debug, Clone)]
pub struct CompletionDelta {
    pub content_delta: Option<String>,
    pub reasoning_delta: Option<String>,
    pub tool_call_delta: Option<ToolCall>,
    pub finish_reason: Option<FinishReason>,
}
