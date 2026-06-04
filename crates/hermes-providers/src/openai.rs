//! `OpenAiProvider` — real OpenAI Chat Completions adapter.
//!
//! Phase 2 minimum: POST `{base_url}/chat/completions` with the
//! serialized request, parse the response, and map the `finish_reason`
//! string to our `FinishReason` enum. Tool-call parsing, streaming,
//! retries, and richer error mapping land in later phases.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use hermes_core::message::{Content, Message, Role};
use hermes_core::provider::{Completion, FinishReason, Provider};
use hermes_core::registry::ToolSchema;
use hermes_core::{ProviderError, Usage};

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
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
        }
    }

    /// Override the API base URL. Tests point this at a local mock
    /// server; users can use it to talk to Azure / Together / a proxy.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<OaiMessage<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OaiTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
}

#[derive(Serialize)]
struct OaiMessage<'a> {
    role: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
    /// Round-trip the LLM's `tool_calls` so the next request
    /// remembers which tools it invoked. OpenAI expects each entry as
    /// `{ id, type: "function", function: { name, arguments } }`.
    /// `arguments` is sent as a JSON *string* (matching how OpenAI
    /// returns it), not a nested object.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OaiToolCallRef<'a>>>,
}

#[derive(Serialize)]
struct OaiToolCallRef<'a> {
    id: &'a str,
    r#type: &'static str, // "function"
    function: OaiFunctionCallRef<'a>,
}

#[derive(Serialize)]
struct OaiFunctionCallRef<'a> {
    name: &'a str,
    arguments: String,
}

#[derive(Serialize)]
struct OaiTool<'a> {
    r#type: &'static str,
    function: OaiFunctionDef<'a>,
}

#[derive(Serialize)]
struct OaiFunctionDef<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a serde_json::Value,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: Option<OaiUsage>,
}

#[derive(Deserialize)]
struct Choice {
    message: OaiRespMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OaiRespMessage {
    content: Option<String>,
    tool_calls: Option<Vec<OaiToolCall>>,
}

#[derive(Deserialize)]
struct OaiToolCall {
    id: String,
    function: OaiFunctionCall,
}

#[derive(Deserialize)]
struct OaiFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct OaiUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn name(&self) -> &str {
        "openai"
    }
    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<Completion, ProviderError> {
        let oai_msgs: Vec<OaiMessage> = messages
            .iter()
            .map(|m| {
                let tool_calls = m.tool_calls.as_ref().map(|calls| {
                    calls
                        .iter()
                        .map(|c| {
                            // OpenAI expects `arguments` as a JSON
                            // *string*, not a nested object — same
                            // shape the response uses.
                            let arguments = serde_json::to_string(&c.arguments)
                                .unwrap_or_else(|_| "null".into());
                            OaiToolCallRef {
                                id: &c.id,
                                r#type: "function",
                                function: OaiFunctionCallRef {
                                    name: &c.name,
                                    arguments,
                                },
                            }
                        })
                        .collect()
                });
                OaiMessage {
                    role: m.role.as_str(),
                    content: match &m.content {
                        Content::Text(s) => Some(s.as_str()),
                        Content::Parts(_) => None,
                    },
                    tool_call_id: m.tool_call_id.as_deref(),
                    tool_calls,
                }
            })
            .collect();

        let oai_tools: Vec<OaiTool> = tools
            .iter()
            .map(|t| OaiTool {
                r#type: "function",
                function: OaiFunctionDef {
                    name: &t.name,
                    description: &t.description,
                    parameters: &t.parameters,
                },
            })
            .collect();

        let has_tools = !oai_tools.is_empty();
        let req = ChatRequest {
            model: &self.model,
            messages: oai_msgs,
            tools: oai_tools,
            tool_choice: if has_tools { Some("auto") } else { None },
        };

        let url = format!("{}/chat/completions", self.base_url);
        let resp = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Err(ProviderError::Cancelled);
            }
            r = self
                .client
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&req)
                .send() => r.map_err(ProviderError::Transport)?,
        };

        if resp.status() == 401 {
            return Err(ProviderError::Auth(resp.text().await.unwrap_or_default()));
        }
        if resp.status() == 429 {
            // Phase 2 minimum: assume 1s backoff. A future phase
            // should read the `retry-after` header.
            return Err(ProviderError::RateLimited {
                retry_after_secs: 1,
            });
        }
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::InvalidResponse(body));
        }

        let parsed: ChatResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::InvalidResponse(e.to_string()))?;

        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ProviderError::InvalidResponse("no choices".into()))?;

        let finish_reason = choice
            .finish_reason
            .as_deref()
            .map(FinishReason::from_provider_str)
            .unwrap_or(FinishReason::Stop);

        let tool_calls = choice.message.tool_calls.map(|calls| {
            calls
                .into_iter()
                .map(|c| hermes_core::message::ToolCall {
                    id: c.id,
                    name: c.function.name,
                    // OpenAI sends `arguments` as a JSON string, not an
                    // object. A garbage payload is treated as Null
                    // rather than failing the whole request — let the
                    // tool's own argument-validation step complain.
                    arguments: serde_json::from_str(&c.function.arguments)
                        .unwrap_or(serde_json::Value::Null),
                })
                .collect()
        });

        let usage = parsed
            .usage
            .map(|u| Usage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
                cached_input_tokens: 0,
            })
            .unwrap_or_default();

        Ok(Completion {
            message: Message {
                role: Role::Assistant,
                content: Content::Text(choice.message.content.unwrap_or_default()),
                reasoning: None,
                tool_call_id: None,
                tool_calls,
            },
            usage,
            finish_reason,
        })
    }
}
