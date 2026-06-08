//! `OpenAiProvider` — the OpenAI Chat Completions adapter.
//!
//! Module layout:
//! - `request` — DTOs + `to_openai_message` / `to_openai_tool` / `build_chat_request`
//! - `sse` — `parse_sse_chunks` + `parse_sse_data_payload` + state
//!
//! The provider struct itself lives in this file because the HTTP client
//! and the `Provider` trait impl are tightly coupled to a single type.

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use perry_hermes_core::message::Message;
use perry_hermes_core::provider::Provider;
use perry_hermes_core::registry::ToolSchema;
use perry_hermes_core::{CompletionStream, ProviderError};

use crate::http::{streaming_client, transport_error_message};

mod request;
mod sse;

pub struct OpenAiProvider {
    api_key: String,
    base_url: String,
    model: String,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://api.openai.com/v1".into(),
            model: model.into(),
            client: streaming_client(),
        }
    }

    /// Override the API base URL. Tests point this at a local mock
    /// server; users can use it to talk to Azure / Together / a proxy.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<CompletionStream, ProviderError> {
        let body = request::build_chat_request(&self.model, messages, tools);
        let url = format!("{}/chat/completions", self.base_url);

        // Pre-flight: send request, race against cancel
        let resp = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Err(ProviderError::Cancelled);
            }
            r = self.client.post(&url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .send() => r.map_err(|e| ProviderError::Transport(transport_error_message(&e)))?,
        };

        // Pre-flight: status check
        if resp.status() == 401 {
            return Err(ProviderError::Auth(resp.text().await.unwrap_or_default()));
        }
        if resp.status() == 429 {
            return Err(ProviderError::RateLimited {
                retry_after_secs: 1,
            });
        }
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::InvalidResponse(body));
        }

        // Hand the byte stream to the SSE parser.
        Ok(Box::pin(sse::parse_sse_chunks(resp.bytes_stream())))
    }
}
