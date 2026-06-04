//! `EchoProvider` — the v0 mock provider.
//!
//! Echoes back the user's last user-role message with an `"echo: "`
//! prefix and `finish_reason = Stop`. Used by phase 1's smoke test and
//! by `hermes --provider echo` for offline iteration on the loop.
//!
//! See `plans/rust-port-design.md` §7.12.

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use hermes_core::message::{Content, Message, Role};
use hermes_core::provider::{Completion, FinishReason, Provider};
use hermes_core::{ProviderError, ToolSchema, Usage};

pub struct EchoProvider {
    name: String,
    model: String,
}

impl EchoProvider {
    pub fn new() -> Self {
        Self {
            name: "echo".into(),
            model: "echo-v0".into(),
        }
    }
}

impl Default for EchoProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for EchoProvider {
    fn name(&self) -> &str {
        &self.name
    }
    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<Completion, ProviderError> {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        // Echo back the last user message.
        let last_user = messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .cloned()
            .unwrap_or(Message {
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
        Ok(Completion {
            message: Message {
                role: Role::Assistant,
                content: Content::Text(reply),
                reasoning: None,
                tool_call_id: None,
                tool_calls: None,
            },
            usage: Usage::default(),
            finish_reason: FinishReason::Stop,
        })
    }
}
