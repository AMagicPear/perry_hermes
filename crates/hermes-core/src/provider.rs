//! The `Provider` trait — the only LLM-facing abstraction in the codebase.

use async_trait::async_trait;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::error::ProviderError;
use crate::message::{Content, Message, Role, ToolCall};
use crate::registry::ToolSchema;
use crate::usage::Usage;

/// Something that can answer a `complete()` call. Every LLM backend
/// (OpenAI, Anthropic, an echo mock, …) implements this.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Stream deltas as the LLM generates them. The consumer drives the
    /// stream to completion (or cancellation), emitting one event per
    /// delta, then assembles a final `Completion` from accumulated state.
    ///
    /// Cancellation contract: the consumer MUST `select!` on the cancel
    /// token and drop the stream when cancelled. Dropping the stream
    /// aborts the in-flight HTTP body.
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<CompletionStream, ProviderError>;

    /// Convenience: drive the stream to a single `Completion`. Default
    /// implementation uses `accumulate_stream`. Providers do not override.
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<Completion, ProviderError> {
        let stream = self.stream(messages, tools, cancel).await?;
        accumulate_stream(stream).await
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
/// chunks carry `arguments_delta` (a JSON string fragment, NOT a parsed
/// value — the consumer concatenates them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments_delta: Option<String>,
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

// ---------------------------------------------------------------------------
// StreamAccumulator
// ---------------------------------------------------------------------------

use std::collections::BTreeMap;

/// Accumulates `CompletionDelta` items from a stream into a final `Completion`.
///
/// Pure data — no async, no I/O. Lives in `hermes-core` so both the trait
/// default `complete()` and `AgentLoop::run` can share it.
pub struct StreamAccumulator {
    content: String,
    reasoning: String,
    tool_calls: BTreeMap<usize, ToolCall>,
    usage: Usage,
    finish_reason: Option<FinishReason>,
}

impl Default for StreamAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamAccumulator {
    pub fn new() -> Self {
        Self {
            content: String::new(),
            reasoning: String::new(),
            tool_calls: BTreeMap::new(),
            usage: Usage::default(),
            finish_reason: None,
        }
    }

    pub fn add(&mut self, delta: &CompletionDelta) {
        if let Some(s) = &delta.content_delta {
            self.content.push_str(s);
        }
        if let Some(s) = &delta.reasoning_delta {
            self.reasoning.push_str(s);
        }
        if let Some(td) = &delta.tool_call_delta {
            let entry = self.tool_calls.entry(td.index).or_insert_with(|| ToolCall {
                id: String::new(),
                name: String::new(),
                arguments: serde_json::Value::Null,
            });
            if let Some(id) = &td.id {
                entry.id = id.clone();
            }
            if let Some(name) = &td.name {
                entry.name = name.clone();
            }
            if let Some(args_frag) = &td.arguments_delta {
                let existing = match &entry.arguments {
                    serde_json::Value::Null => String::new(),
                    serde_json::Value::String(s) => s.clone(),
                    v => v.to_string(),
                };
                let combined = format!("{existing}{args_frag}");
                entry.arguments = serde_json::Value::String(combined);
            }
        }
        if let Some(u) = delta.usage {
            self.usage = u;
        }
        if let Some(fr) = delta.finish_reason {
            self.finish_reason = Some(fr);
        }
    }

    /// Build the final `Completion`. If `finish_reason` was never set
    /// (stream ended with `None`), defaults to `FinishReason::Stop`.
    pub fn finalize(mut self) -> Completion {
        for tc in self.tool_calls.values_mut() {
            if let serde_json::Value::String(s) = &tc.arguments {
                if let Ok(parsed) = serde_json::from_str(s) {
                    tc.arguments = parsed;
                }
            }
        }
        let finish_reason = self.finish_reason.unwrap_or(FinishReason::Stop);
        let tool_calls = if self.tool_calls.is_empty() {
            None
        } else {
            Some(self.tool_calls.into_values().collect())
        };
        let message = Message {
            role: Role::Assistant,
            content: Content::Text(std::mem::take(&mut self.content)),
            reasoning: if self.reasoning.is_empty() {
                None
            } else {
                Some(std::mem::take(&mut self.reasoning))
            },
            tool_call_id: None,
            tool_calls,
        };
        Completion {
            message,
            usage: self.usage,
            finish_reason,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.content.is_empty() && self.reasoning.is_empty() && self.tool_calls.is_empty()
    }

    /// Build a `Message` for the cancellation path. Filters out tool calls
    /// whose accumulated arguments are not valid JSON. Caller checks
    /// `is_empty()` before deciding to push into history.
    pub fn into_partial_message(mut self, role: Role) -> Message {
        self.tool_calls.retain(|_, tc| {
            if let serde_json::Value::String(s) = &tc.arguments {
                serde_json::from_str::<serde_json::Value>(s).is_ok()
            } else {
                true
            }
        });
        let tool_calls = if self.tool_calls.is_empty() {
            None
        } else {
            Some(self.tool_calls.into_values().collect())
        };
        Message {
            role,
            content: Content::Text(self.content),
            reasoning: if self.reasoning.is_empty() {
                None
            } else {
                Some(self.reasoning)
            },
            tool_call_id: None,
            tool_calls,
        }
    }
}

/// Drive a `CompletionStream` to completion and return the final `Completion`.
///
/// This is a public helper used by the default `Provider::complete` impl.
/// It does NOT emit per-delta events — for that, use `AgentLoop::run` which
/// has its own private drive loop.
pub async fn accumulate_stream(mut stream: CompletionStream) -> Result<Completion, ProviderError> {
    let mut acc = StreamAccumulator::new();
    while let Some(item) = stream.next().await {
        let delta = item?;
        acc.add(&delta);
    }
    Ok(acc.finalize())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Content;

    fn delta(content: Option<&str>, reasoning: Option<&str>) -> CompletionDelta {
        CompletionDelta {
            content_delta: content.map(String::from),
            reasoning_delta: reasoning.map(String::from),
            tool_call_delta: None,
            usage: None,
            finish_reason: None,
        }
    }

    #[tokio::test]
    async fn accumulate_stream_returns_completion() {
        use futures::stream;
        let deltas = vec![
            Ok(CompletionDelta {
                content_delta: Some("Hello".into()),
                reasoning_delta: None,
                tool_call_delta: None,
                usage: None,
                finish_reason: None,
            }),
            Ok(CompletionDelta {
                content_delta: Some(" world".into()),
                reasoning_delta: None,
                tool_call_delta: None,
                usage: Some(Usage {
                    input_tokens: 1,
                    output_tokens: 2,
                    cached_input_tokens: 0,
                }),
                finish_reason: Some(FinishReason::Stop),
            }),
        ];
        let stream: CompletionStream = Box::pin(stream::iter(deltas));
        let completion = accumulate_stream(stream).await.unwrap();
        assert_eq!(completion.finish_reason, FinishReason::Stop);
        assert!(matches!(completion.message.content, Content::Text(ref s) if s == "Hello world"));
        assert_eq!(completion.usage.output_tokens, 2);
    }

    #[tokio::test]
    async fn accumulate_stream_defaults_to_stop_on_none() {
        use futures::stream;
        let deltas = vec![Ok(CompletionDelta {
            content_delta: Some("hi".into()),
            reasoning_delta: None,
            tool_call_delta: None,
            usage: None,
            finish_reason: None,
        })];
        let stream: CompletionStream = Box::pin(stream::iter(deltas));
        let completion = accumulate_stream(stream).await.unwrap();
        assert_eq!(completion.finish_reason, FinishReason::Stop);
    }

    #[tokio::test]
    async fn accumulate_stream_propagates_error() {
        use futures::stream;
        let deltas: Vec<Result<CompletionDelta, ProviderError>> = vec![
            Ok(CompletionDelta {
                content_delta: Some("a".into()),
                reasoning_delta: None,
                tool_call_delta: None,
                usage: None,
                finish_reason: None,
            }),
            Err(ProviderError::InvalidResponse("boom".into())),
        ];
        let stream: CompletionStream = Box::pin(stream::iter(deltas));
        let result = accumulate_stream(stream).await;
        assert!(matches!(result, Err(ProviderError::InvalidResponse(_))));
    }

    #[test]
    fn empty_accumulator_finalizes_as_stop() {
        let mut acc = StreamAccumulator::new();
        acc.add(&delta(None, None));
        let completion = acc.finalize();
        assert_eq!(completion.finish_reason, FinishReason::Stop);
        assert!(matches!(completion.message.content, Content::Text(ref s) if s.is_empty()));
        assert!(completion.message.tool_calls.is_none());
    }

    #[test]
    fn content_concatenates_across_deltas() {
        let mut acc = StreamAccumulator::new();
        acc.add(&delta(Some("Hel"), None));
        acc.add(&delta(Some("lo "), None));
        // Fixed bug: plan had `Some(None)` which accumulates "None" into reasoning
        acc.add(&delta(Some("world"), None));
        acc.add(&delta(None, Some("thinking...")));
        let completion = acc.finalize();
        assert!(matches!(completion.message.content, Content::Text(ref s) if s == "Hello world"));
        assert_eq!(completion.message.reasoning.as_deref(), Some("thinking..."));
    }

    #[test]
    fn tool_calls_accumulate_by_index() {
        let mut acc = StreamAccumulator::new();
        acc.add(&CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: Some(ToolCallDelta {
                index: 0,
                id: Some("call_a".into()),
                name: Some("bash".into()),
                arguments_delta: Some("{\"command\":".into()),
            }),
            usage: None,
            finish_reason: None,
        });
        acc.add(&CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: Some(ToolCallDelta {
                index: 0,
                id: None,
                name: None,
                arguments_delta: Some("\"ls\"}".into()),
            }),
            usage: None,
            finish_reason: None,
        });
        acc.add(&CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: Some(ToolCallDelta {
                index: 1,
                id: Some("call_b".into()),
                name: Some("bash".into()),
                arguments_delta: Some("{\"command\":\"pwd\"}".into()),
            }),
            usage: None,
            finish_reason: Some(FinishReason::ToolUse),
        });
        let completion = acc.finalize();
        assert_eq!(completion.finish_reason, FinishReason::ToolUse);
        let calls = completion.message.tool_calls.expect("two tool calls");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_a");
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[0].arguments, serde_json::json!({"command": "ls"}));
        assert_eq!(calls[1].id, "call_b");
        assert_eq!(calls[1].arguments, serde_json::json!({"command": "pwd"}));
    }

    #[test]
    fn is_empty_true_for_no_deltas() {
        let acc = StreamAccumulator::new();
        assert!(acc.is_empty());
    }

    #[test]
    fn is_empty_false_after_content_delta() {
        let mut acc = StreamAccumulator::new();
        acc.add(&delta(Some("hi"), None));
        assert!(!acc.is_empty());
    }

    #[test]
    fn into_partial_message_drops_incomplete_tool_calls() {
        let mut acc = StreamAccumulator::new();
        acc.add(&delta(Some("text so far"), None));
        acc.add(&CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: Some(ToolCallDelta {
                index: 0,
                id: Some("call_a".into()),
                name: Some("bash".into()),
                arguments_delta: Some("{\"command\":\"ls\"}".into()),
            }),
            usage: None,
            finish_reason: None,
        });
        acc.add(&CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: Some(ToolCallDelta {
                index: 1,
                id: Some("call_b".into()),
                name: Some("bash".into()),
                arguments_delta: Some("{\"comm".into()),
            }),
            usage: None,
            finish_reason: None,
        });
        let partial = acc.into_partial_message(Role::Assistant);
        let calls = partial.tool_calls.expect("one complete call kept");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_a");
    }
}
