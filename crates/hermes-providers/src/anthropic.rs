use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use hermes_core::message::{Content, ContentPart, Message, Role};
use hermes_core::provider::{CompletionDelta, FinishReason, Provider, ToolCallDelta};
use hermes_core::registry::ToolSchema;
use hermes_core::{CompletionStream, ProviderError, Usage};

pub struct AnthropicProvider {
    api_key: String,
    api_key_header: String,
    model: String,
    base_url: String,
    request_options: AnthropicRequestOptions,
    client: reqwest::Client,
}

#[derive(Debug, Clone, Default)]
pub struct AnthropicRequestOptions {
    pub thinking: Option<AnthropicThinking>,
}

#[derive(Debug, Clone)]
pub enum AnthropicThinking {
    Manual {
        budget_tokens: u32,
    },
    Adaptive {
        display: String,
        effort: Option<String>,
    },
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

#[derive(Serialize)]
struct MessagesRequest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<WireToolChoice>,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingParam>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<OutputConfig>,
}

#[derive(Serialize)]
struct OutputConfig {
    effort: String,
}

#[derive(Debug, Clone, Serialize)]
struct WireMessage {
    role: String,
    content: WireMessageContent,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum WireMessageContent {
    Text(String),
    Blocks(Vec<WireContentBlock>),
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

#[derive(Serialize)]
struct WireTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireToolChoice {
    Auto,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ThinkingParam {
    Enabled { budget_tokens: u32 },
    Adaptive { display: String },
}

fn build_request_body_with_options(
    model: &str,
    messages: &[Message],
    tools: &[ToolSchema],
    stream: bool,
    options: AnthropicRequestOptions,
) -> MessagesRequest {
    let (system, messages) = convert_messages_to_anthropic(messages);
    let tools = convert_tools(tools);
    let has_tools = !tools.is_empty();
    let manual_thinking = matches!(options.thinking, Some(AnthropicThinking::Manual { .. }));
    let output_config = build_output_config(&options.thinking);

    MessagesRequest {
        model: model.to_string(),
        system,
        messages,
        tools,
        tool_choice: if has_tools {
            Some(WireToolChoice::Auto)
        } else {
            None
        },
        max_tokens: 16_384,
        stream,
        thinking: build_thinking_param(options.thinking),
        temperature: if manual_thinking { Some(1.0) } else { None },
        output_config,
    }
}

fn convert_tools(tools: &[ToolSchema]) -> Vec<WireTool> {
    tools
        .iter()
        .map(|t| WireTool {
            name: t.name.clone(),
            description: t.description.clone(),
            input_schema: t.parameters.clone(),
        })
        .collect()
}

fn convert_messages_to_anthropic(messages: &[Message]) -> (Option<String>, Vec<WireMessage>) {
    let mut system = None;
    let mut wire = Vec::new();
    let mut pending_tool_results: Vec<WireContentBlock> = Vec::new();

    for message in messages {
        if !pending_tool_results.is_empty() && message.role != Role::Tool {
            flush_tool_results(&mut wire, &mut pending_tool_results);
        }

        match message.role {
            Role::System => {
                system = Some(content_to_text(&message.content));
            }
            Role::User => {
                wire.push(WireMessage {
                    role: "user".into(),
                    content: content_to_wire_user(&message.content),
                });
            }
            Role::Assistant => {
                let mut blocks = Vec::new();
                let text = content_to_text(&message.content);
                if !text.is_empty() {
                    blocks.push(WireContentBlock::Text { text });
                }
                if let Some(calls) = &message.tool_calls {
                    for call in calls {
                        blocks.push(WireContentBlock::ToolUse {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            input: call.arguments.clone(),
                        });
                    }
                }

                wire.push(WireMessage {
                    role: "assistant".into(),
                    content: WireMessageContent::Blocks(blocks),
                });
            }
            Role::Tool => {
                pending_tool_results.push(WireContentBlock::ToolResult {
                    tool_use_id: message.tool_call_id.clone().unwrap_or_default(),
                    content: content_to_text(&message.content),
                    is_error: None,
                });
            }
        }
    }

    flush_tool_results(&mut wire, &mut pending_tool_results);
    (system, wire)
}

fn flush_tool_results(wire: &mut Vec<WireMessage>, pending: &mut Vec<WireContentBlock>) {
    if pending.is_empty() {
        return;
    }

    let results = std::mem::take(pending);
    if let Some(last) = wire.last_mut() {
        if last.role == "user" {
            match &mut last.content {
                WireMessageContent::Text(text) => {
                    let mut blocks = vec![WireContentBlock::Text {
                        text: std::mem::take(text),
                    }];
                    blocks.extend(results);
                    last.content = WireMessageContent::Blocks(blocks);
                }
                WireMessageContent::Blocks(blocks) => blocks.extend(results),
            }
            return;
        }
    }

    wire.push(WireMessage {
        role: "user".into(),
        content: WireMessageContent::Blocks(results),
    });
}

fn content_to_wire_user(content: &Content) -> WireMessageContent {
    WireMessageContent::Text(content_to_text(content))
}

fn content_to_text(content: &Content) -> String {
    match content {
        Content::Text(s) => s.clone(),
        Content::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.clone()),
                ContentPart::ImageUrl { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn build_thinking_param(thinking: Option<AnthropicThinking>) -> Option<ThinkingParam> {
    match thinking {
        Some(AnthropicThinking::Manual { budget_tokens }) => {
            Some(ThinkingParam::Enabled { budget_tokens })
        }
        Some(AnthropicThinking::Adaptive { display, effort: _ }) => {
            Some(ThinkingParam::Adaptive { display })
        }
        None => None,
    }
}

fn build_output_config(thinking: &Option<AnthropicThinking>) -> Option<OutputConfig> {
    match thinking {
        Some(AnthropicThinking::Adaptive {
            effort: Some(effort),
            ..
        }) => Some(OutputConfig {
            effort: effort.clone(),
        }),
        _ => None,
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
        let body = build_request_body_with_options(
            &self.model,
            messages,
            tools,
            true,
            self.request_options.clone(),
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
            return Err(ProviderError::RateLimited {
                retry_after_secs: 1,
            });
        }
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::InvalidResponse(body));
        }

        Ok(Box::pin(parse_sse_chunks(resp.bytes_stream())))
    }
}

#[derive(Default)]
struct AnthropicStreamState {
    usage: Usage,
}

fn parse_sse_chunks(
    bytes: impl Stream<Item = reqwest::Result<Bytes>> + Unpin,
) -> impl Stream<Item = Result<CompletionDelta, ProviderError>> {
    async_stream::stream! {
        let mut buffer = String::new();
        let mut state = AnthropicStreamState::default();
        let mut bytes = Box::pin(bytes);
        while let Some(chunk) = bytes.next().await {
            match chunk {
                Ok(c) => buffer.push_str(&String::from_utf8_lossy(&c)),
                Err(e) => { yield Err(ProviderError::Transport(e.to_string())); return; }
            }
            while let Some(pos) = buffer.find("\n\n") {
                let event: String = buffer.drain(..pos + 2).collect();
                let payload = event
                    .lines()
                    .filter_map(|line| line.strip_prefix("data: "))
                    .collect::<Vec<_>>()
                    .join("\n");
                if payload.is_empty() {
                    continue;
                }
                match parse_sse_data_payload(&payload, &mut state) {
                    Ok(Some(delta)) => yield Ok(delta),
                    Ok(None) => {}
                    Err(e) => { yield Err(e); return; }
                }
            }
        }
    }
}

fn parse_sse_data_payload(
    payload: &str,
    state: &mut AnthropicStreamState,
) -> Result<Option<CompletionDelta>, ProviderError> {
    let value: serde_json::Value = serde_json::from_str(payload)
        .map_err(|e| ProviderError::InvalidResponse(format!("sse json: {e}")))?;
    let event_type = value
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProviderError::InvalidResponse("missing Anthropic SSE type".into()))?;

    match event_type {
        "message_start" => {
            if let Some(usage) = value.get("message").and_then(|m| m.get("usage")) {
                state.usage.input_tokens = usage
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_default();
                state.usage.output_tokens = usage
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_default();
                state.usage.cached_input_tokens = cached_input_tokens_from_anthropic_usage(usage);
            }
            Ok(Some(usage_delta(state.usage)))
        }
        "content_block_start" => parse_content_block_start(&value),
        "content_block_delta" => parse_content_block_delta(&value),
        "content_block_stop" | "message_stop" | "ping" => Ok(None),
        "message_delta" => {
            if let Some(usage) = value.get("usage") {
                if let Some(input) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                    state.usage.input_tokens = input;
                }
                if let Some(output) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                    state.usage.output_tokens = output;
                }
                let cached = cached_input_tokens_from_anthropic_usage(usage);
                if cached > 0 {
                    state.usage.cached_input_tokens = cached;
                }
            }
            let finish_reason = value
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(|v| v.as_str())
                .map(anthropic_finish_reason);
            Ok(Some(CompletionDelta {
                content_delta: None,
                reasoning_delta: None,
                tool_call_delta: None,
                usage: Some(state.usage),
                finish_reason,
            }))
        }
        "error" => {
            let msg = value
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("Anthropic stream error");
            Err(ProviderError::InvalidResponse(msg.into()))
        }
        other => Err(ProviderError::InvalidResponse(format!(
            "unknown Anthropic SSE event: {other}"
        ))),
    }
}

fn parse_content_block_start(
    value: &serde_json::Value,
) -> Result<Option<CompletionDelta>, ProviderError> {
    let block = value
        .get("content_block")
        .ok_or_else(|| ProviderError::InvalidResponse("missing content_block".into()))?;
    match block.get("type").and_then(|v| v.as_str()) {
        Some("tool_use") => Ok(Some(CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: Some(ToolCallDelta {
                index: value
                    .get("index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_default() as usize,
                id: block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned),
                name: block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned),
                arguments_delta: None,
            }),
            usage: None,
            finish_reason: None,
        })),
        Some("text" | "thinking") => Ok(None),
        Some(other) => Err(ProviderError::InvalidResponse(format!(
            "unknown Anthropic content block: {other}"
        ))),
        None => Err(ProviderError::InvalidResponse(
            "missing content_block type".into(),
        )),
    }
}

fn parse_content_block_delta(
    value: &serde_json::Value,
) -> Result<Option<CompletionDelta>, ProviderError> {
    let delta = value
        .get("delta")
        .ok_or_else(|| ProviderError::InvalidResponse("missing content_block delta".into()))?;
    match delta.get("type").and_then(|v| v.as_str()) {
        Some("text_delta") => Ok(Some(CompletionDelta {
            content_delta: delta
                .get("text")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned),
            reasoning_delta: None,
            tool_call_delta: None,
            usage: None,
            finish_reason: None,
        })),
        Some("input_json_delta") => Ok(Some(CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: Some(ToolCallDelta {
                index: value
                    .get("index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_default() as usize,
                id: None,
                name: None,
                arguments_delta: delta
                    .get("partial_json")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned),
            }),
            usage: None,
            finish_reason: None,
        })),
        Some("thinking_delta") => Ok(Some(CompletionDelta {
            content_delta: None,
            reasoning_delta: delta
                .get("thinking")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned),
            tool_call_delta: None,
            usage: None,
            finish_reason: None,
        })),
        Some("signature_delta") => Ok(None),
        Some(other) => Err(ProviderError::InvalidResponse(format!(
            "unknown Anthropic delta: {other}"
        ))),
        None => Err(ProviderError::InvalidResponse(
            "missing content_block delta type".into(),
        )),
    }
}

fn usage_delta(usage: Usage) -> CompletionDelta {
    CompletionDelta {
        content_delta: None,
        reasoning_delta: None,
        tool_call_delta: None,
        usage: Some(usage),
        finish_reason: None,
    }
}

fn cached_input_tokens_from_anthropic_usage(usage: &serde_json::Value) -> u64 {
    let cache_read = usage
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or_default();
    let cache_creation = usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or_default();
    cache_read + cache_creation
}

fn anthropic_finish_reason(s: &str) -> FinishReason {
    match s {
        "end_turn" | "stop_sequence" => FinishReason::Stop,
        "tool_use" => FinishReason::ToolUse,
        "max_tokens" => FinishReason::Length,
        "refusal" => FinishReason::ContentFilter,
        _ => FinishReason::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

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

        let (system, wire) = convert_messages_to_anthropic(&messages);

        assert_eq!(system.as_deref(), Some("system"));
        assert_eq!(wire.len(), 1);
        match &wire[0].content {
            WireMessageContent::Text(text) => assert_eq!(text, "a\nb"),
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
            tool_calls: Some(vec![hermes_core::message::ToolCall {
                id: "toolu_1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({ "command": "ls" }),
            }]),
        };
        let tool = Message {
            role: Role::Tool,
            content: Content::Text("ok".into()),
            reasoning: None,
            tool_call_id: Some("toolu_1".into()),
            tool_calls: None,
        };

        let (_, wire) = convert_messages_to_anthropic(&[assistant, tool]);

        assert_eq!(wire.len(), 2);
        match &wire[0].content {
            WireMessageContent::Blocks(blocks) => {
                assert!(matches!(blocks[0], WireContentBlock::ToolUse { .. }));
            }
            _ => panic!("expected assistant blocks"),
        }
        match &wire[1].content {
            WireMessageContent::Blocks(blocks) => {
                assert!(matches!(blocks[0], WireContentBlock::ToolResult { .. }));
            }
            _ => panic!("expected tool result blocks"),
        }
    }

    #[test]
    fn request_body_uses_structured_tool_choice_and_input_schema() {
        let body = build_request_body_with_options(
            "claude-sonnet-4-5",
            &[msg(Role::User, Content::Text("hi".into()))],
            &[ToolSchema {
                name: "bash".into(),
                description: "Run a shell command".into(),
                parameters: serde_json::json!({ "type": "object" }),
            }],
            true,
            AnthropicRequestOptions::default(),
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
        let body = build_request_body_with_options(
            "claude-3-7-sonnet-latest",
            &[],
            &[],
            true,
            AnthropicRequestOptions::default(),
        );
        let json = serde_json::to_value(body).unwrap();
        assert!(json.get("thinking").is_none());
        assert!(json.get("temperature").is_none());
    }

    #[test]
    fn manual_thinking_is_explicit() {
        let body = build_request_body_with_options(
            "claude-3-7-sonnet-latest",
            &[],
            &[],
            true,
            AnthropicRequestOptions {
                thinking: Some(AnthropicThinking::Manual {
                    budget_tokens: 8_000,
                }),
            },
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
        let body = build_request_body_with_options(
            "claude-opus-4-8",
            &[],
            &[],
            true,
            AnthropicRequestOptions {
                thinking: Some(AnthropicThinking::Adaptive {
                    display: "summarized".into(),
                    effort: None,
                }),
            },
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
        let body = build_request_body_with_options(
            "claude-opus-4-8",
            &[],
            &[],
            true,
            AnthropicRequestOptions {
                thinking: Some(AnthropicThinking::Adaptive {
                    display: "summarized".into(),
                    effort: Some("medium".into()),
                }),
            },
        );
        let json = serde_json::to_value(body).unwrap();
        assert_eq!(
            json["output_config"],
            serde_json::json!({ "effort": "medium" })
        );
    }

    #[test]
    fn parses_text_tool_and_usage_stream() {
        let input = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":0}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n\n";
        let s = parse_sse_chunks(stream::iter(vec![Ok::<_, reqwest::Error>(
            Bytes::copy_from_slice(input),
        )]));

        let deltas = futures::executor::block_on(async move {
            let mut out = Vec::new();
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                out.push(item.unwrap());
            }
            out
        });

        assert_eq!(deltas[0].usage.unwrap().input_tokens, 2);
        assert_eq!(deltas[1].content_delta.as_deref(), Some("Hi"));
        assert_eq!(deltas[2].usage.unwrap().output_tokens, 4);
        assert_eq!(deltas[2].finish_reason, Some(FinishReason::Stop));
    }

    #[test]
    fn message_delta_usage_can_update_input_tokens() {
        let input = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"output_tokens\":0}}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":8,\"output_tokens\":4}}\n\n";
        let s = parse_sse_chunks(stream::iter(vec![Ok::<_, reqwest::Error>(
            Bytes::copy_from_slice(input),
        )]));

        let deltas = futures::executor::block_on(async move {
            let mut out = Vec::new();
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                out.push(item.unwrap());
            }
            out
        });

        let usage = deltas[1].usage.unwrap();
        assert_eq!(usage.input_tokens, 8);
        assert_eq!(usage.output_tokens, 4);
    }

    #[test]
    fn chunks_split_across_frames_assemble_correctly() {
        use futures::stream;
        let chunks: Vec<&[u8]> = vec![
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n",
            b"\n",
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n",
        ];
        let byte_stream = stream::iter(
            chunks
                .iter()
                .map(|c| Ok::<_, reqwest::Error>(Bytes::copy_from_slice(c)))
                .collect::<Vec<_>>(),
        );
        let s = parse_sse_chunks(byte_stream);
        let deltas: Vec<CompletionDelta> = futures::executor::block_on(async move {
            let mut v = Vec::new();
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                v.push(item.unwrap());
            }
            v
        });
        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].content_delta.as_deref(), Some("Hel"));
        assert_eq!(deltas[1].content_delta.as_deref(), Some("lo"));
    }

    #[test]
    fn message_stop_event_terminates_cleanly() {
        // Anthropic uses explicit event types: message_delta carries
        // stop_reason + usage, then message_stop is the actual end
        // marker. The parser must yield the message_delta delta and
        // then return cleanly when message_stop arrives (no extra
        // Ok(None) leak).
        let input = b"\
event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":0}}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n\
event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n\
";
        let s = parse_sse_chunks(stream::iter(vec![Ok::<_, reqwest::Error>(
            Bytes::copy_from_slice(input),
        )]));

        let deltas = futures::executor::block_on(async move {
            let mut out = Vec::new();
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                out.push(item.unwrap());
            }
            out
        });

        // Expect: message_start yields usage delta, content_block_delta
        // yields text delta, message_delta yields finish_reason + usage.
        // message_stop is silently consumed and yields nothing extra.
        assert_eq!(deltas.len(), 3);
        assert_eq!(deltas[0].usage.unwrap().input_tokens, 2);
        assert_eq!(deltas[1].content_delta.as_deref(), Some("Hi"));
        assert_eq!(deltas[2].finish_reason, Some(FinishReason::Stop));
        assert_eq!(deltas[2].usage.unwrap().output_tokens, 4);
    }

    #[test]
    fn partial_utf8_in_a_chunk_does_not_panic() {
        use futures::stream;
        let s = parse_sse_chunks(stream::iter(vec![Ok::<_, reqwest::Error>(
            Bytes::copy_from_slice(b"data: \xFF\xFE\n\n"),
        )]));
        let result: Result<Vec<CompletionDelta>, ProviderError> =
            futures::executor::block_on(async move {
                let mut v = Vec::new();
                futures::pin_mut!(s);
                while let Some(item) = s.next().await {
                    v.push(item?);
                }
                Ok(v)
            });
        // Smoke: does not panic; returns Ok(empty) or Err cleanly.
        let _ = result;
    }
}
