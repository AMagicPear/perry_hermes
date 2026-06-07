//! `AnthropicProvider` — the Anthropic Messages API adapter.
//!
//! Module layout:
//! - `request` — DTOs + `to_anthropic_messages` / `to_anthropic_tools` / `build_messages_request`
//! - `sse` — `parse_sse_chunks` + `parse_sse_data_payload` + state
//!
//! The provider struct + the `Provider` trait impl live here because the
//! HTTP client is tightly coupled to a single type.

use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use hermes_core::message::Message;
use hermes_core::provider::Provider;
use hermes_core::registry::ToolSchema;
use hermes_core::{CompletionStream, ProviderError};

mod request;
mod sse;

/// Per-request options for the Anthropic adapter. Currently only carries
/// the thinking mode (manual / adaptive), but new fields are expected as
/// the API grows.
#[derive(Debug, Clone, Default)]
pub struct AnthropicRequestOptions {
    pub thinking: Option<AnthropicThinking>,
}

/// The thinking-mode configuration for an Anthropic request. Maps to
/// the `thinking` + `output_config` + `temperature` fields on the
/// request body.
#[derive(Debug, Clone)]
pub enum AnthropicThinking {
    /// Fixed-budget extended thinking. Forces `temperature=1.0`.
    Manual { budget_tokens: u32 },
    /// Server-side adaptive thinking with display hints and optional
    /// effort. Forces `temperature` to be unset.
    Adaptive {
        display: String,
        effort: Option<String>,
    },
}

pub struct AnthropicProvider {
    api_key: String,
    api_key_header: String,
    model: String,
    base_url: String,
    request_options: AnthropicRequestOptions,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            api_key_header: "x-api-key".into(),
            model: model.into(),
            base_url: "https://api.anthropic.com/v1".into(),
            request_options: AnthropicRequestOptions::default(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn with_api_key_header(mut self, header_name: impl Into<String>) -> Self {
        self.api_key_header = header_name.into();
        self
    }

    pub fn with_request_options(mut self, options: AnthropicRequestOptions) -> Self {
        self.request_options = options;
        self
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<CompletionStream, ProviderError> {
        let body = request::build_messages_request(
            &self.model,
            messages,
            tools,
            true,
            self.request_options.thinking.clone(),
        );
        let url = format!("{}/messages", self.base_url);

        let resp = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Err(ProviderError::Cancelled);
            }
            r = self.client.post(&url)
                .header(&self.api_key_header, &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&body)
                .send() => r.map_err(|e| ProviderError::Transport(e.to_string()))?,
        };

        if resp.status() == 401 {
            return Err(ProviderError::Auth(resp.text().await.unwrap_or_default()));
        }
        if resp.status() == 429 {
            return Err(ProviderError::RateLimited { retry_after_secs: 1 });
        }
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::InvalidResponse(body));
        }

        Ok(Box::pin(sse::parse_sse_chunks(resp.bytes_stream())))
    }
}

#[cfg(test)]
mod tests {
    //! Tests for the request builder that don't fit the SSE-parsing module.
    use super::*;
    use hermes_core::message::{Content, ContentPart, Message, Role, ToolCall};
    use hermes_core::registry::ToolSchema;

    fn msg(role: Role, content: Content) -> Message {
        Message {
            role,
            content,
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    #[test]
    fn convert_messages_pulls_system_out_and_joins_text_parts() {
        let messages = vec![
            msg(Role::System, Content::Text("system".into())),
            msg(
                Role::User,
                Content::Parts(vec![
                    ContentPart::Text { text: "a".into() },
                    ContentPart::ImageUrl {
                        url: "https://example.com/image.png".into(),
                    },
                    ContentPart::Text { text: "b".into() },
                ]),
            ),
        ];

        let (system, wire) = request::to_anthropic_messages(&messages);

        assert_eq!(system.as_deref(), Some("system"));
        assert_eq!(wire.len(), 1);
        match &wire[0].content {
            request::AnthropicMessageContent::Text(text) => assert_eq!(text, "a\nb"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn convert_messages_emits_tool_use_and_tool_result_blocks() {
        let assistant = Message {
            role: Role::Assistant,
            content: Content::Text("".into()),
            reasoning: Some("not serialized without signatures".into()),
            tool_call_id: None,
            tool_calls: Some(vec![ToolCall::new(
                "toolu_1",
                "bash",
                serde_json::json!({ "command": "ls" }),
            )]),
        };
        let tool = Message {
            role: Role::Tool,
            content: Content::Text("ok".into()),
            reasoning: None,
            tool_call_id: Some("toolu_1".into()),
            tool_calls: None,
        };

        let (_, wire) = request::to_anthropic_messages(&[assistant, tool]);

        assert_eq!(wire.len(), 2);
        match &wire[0].content {
            request::AnthropicMessageContent::Blocks(blocks) => {
                assert!(matches!(
                    blocks[0],
                    request::AnthropicContentBlock::ToolUse { .. }
                ));
            }
            _ => panic!("expected assistant blocks"),
        }
        match &wire[1].content {
            request::AnthropicMessageContent::Blocks(blocks) => {
                assert!(matches!(
                    blocks[0],
                    request::AnthropicContentBlock::ToolResult { .. }
                ));
            }
            _ => panic!("expected tool result blocks"),
        }
    }

    #[test]
    fn request_body_uses_structured_tool_choice_and_input_schema() {
        let body = request::build_messages_request(
            "claude-sonnet-4-5",
            &[msg(Role::User, Content::Text("hi".into()))],
            &[ToolSchema {
                name: "bash".into(),
                description: "Run a shell command".into(),
                parameters: serde_json::json!({ "type": "object" }),
            }],
            true,
            None,
        );

        let json = serde_json::to_value(body).unwrap();
        assert_eq!(json["tool_choice"], serde_json::json!({ "type": "auto" }));
        assert_eq!(
            json["tools"][0]["input_schema"],
            serde_json::json!({ "type": "object" })
        );
        assert!(json["tools"][0].get("parameters").is_none());
    }

    #[test]
    fn thinking_defaults_to_off_for_claude_3_7() {
        let body =
            request::build_messages_request("claude-3-7-sonnet-latest", &[], &[], true, None);
        let json = serde_json::to_value(body).unwrap();
        assert!(json.get("thinking").is_none());
        assert!(json.get("temperature").is_none());
    }

    #[test]
    fn manual_thinking_is_explicit() {
        let body = request::build_messages_request(
            "claude-3-7-sonnet-latest",
            &[],
            &[],
            true,
            Some(AnthropicThinking::Manual { budget_tokens: 8_000 }),
        );
        let json = serde_json::to_value(body).unwrap();
        assert_eq!(
            json["thinking"],
            serde_json::json!({ "type": "enabled", "budget_tokens": 8000 })
        );
        assert_eq!(json["temperature"], serde_json::json!(1.0));
    }

    #[test]
    fn adaptive_thinking_is_explicit() {
        let body = request::build_messages_request(
            "claude-opus-4-8",
            &[],
            &[],
            true,
            Some(AnthropicThinking::Adaptive {
                display: "summarized".into(),
                effort: None,
            }),
        );
        let json = serde_json::to_value(body).unwrap();
        assert_eq!(
            json["thinking"],
            serde_json::json!({ "type": "adaptive", "display": "summarized" })
        );
        assert!(json.get("temperature").is_none());
    }

    #[test]
    fn adaptive_thinking_can_set_effort() {
        let body = request::build_messages_request(
            "claude-opus-4-8",
            &[],
            &[],
            true,
            Some(AnthropicThinking::Adaptive {
                display: "summarized".into(),
                effort: Some("medium".into()),
            }),
        );
        let json = serde_json::to_value(body).unwrap();
        assert_eq!(
            json["output_config"],
            serde_json::json!({ "effort": "medium" })
        );
    }
}
