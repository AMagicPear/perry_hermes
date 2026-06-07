//! Echo provider — yields a single `echo: text` delta and stops.
//! Useful for offline smoke tests of the agent loop without an API key.

use async_trait::async_trait;
use futures::stream;
use hermes_core::{
    message::{Content, Message, Role},
    provider::{CompletionDelta, CompletionStream, FinishReason, Provider},
    registry::ToolSchema,
    ProviderError, Usage,
};
use tokio_util::sync::CancellationToken;

pub struct EchoProvider;

impl EchoProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EchoProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for EchoProvider {
    async fn stream(
        &self,
        messages: &[Message],
        _tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<CompletionStream, ProviderError> {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let last_user = messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .cloned()
            .unwrap_or_else(|| Message {
                role: Role::Assistant,
                content: Content::Text("(nothing to echo)".into()),
                reasoning: None,
                tool_call_id: None,
                tool_calls: None,
            });
        let reply = match &last_user.content {
            Content::Text(t) => format!("echo: {t}"),
            Content::Parts(_) => "echo: (multimodal)".to_string(),
        };
        let delta = CompletionDelta {
            content_delta: Some(reply),
            reasoning_delta: None,
            tool_call_delta: None,
            usage: Some(Usage::default()),
            finish_reason: Some(FinishReason::Stop),
        };
        Ok(Box::pin(stream::once(async move { Ok(delta) })))
    }
}
