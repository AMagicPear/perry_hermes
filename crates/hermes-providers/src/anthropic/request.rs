//! Request DTOs and the `build_messages_request` builder for the
//! Anthropic Messages API.
//!
//! DTO names are prefixed `Anthropic*` (not `Wire*`) so they read as
//! "the Anthropic thing" at a glance. The `to_anthropic_*` free
//! functions translate from `hermes_core` types.

use serde::Serialize;

use hermes_core::message::{Content, ContentPart, Message, Role};
use hermes_core::registry::ToolSchema;

use crate::anthropic::AnthropicThinking;

/// The full Anthropic Messages API request body, serialized as JSON.
#[derive(Serialize)]
pub(super) struct AnthropicMessagesRequest {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<AnthropicTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<AnthropicToolChoice>,
    pub max_tokens: u32,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<AnthropicThinkingParam>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<AnthropicOutputConfig>,
}

#[derive(Serialize)]
pub(super) struct AnthropicOutputConfig {
    pub effort: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct AnthropicMessage {
    pub role: String,
    pub content: AnthropicMessageContent,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub(super) enum AnthropicMessageContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum AnthropicContentBlock {
    Text { text: String },
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
pub(super) struct AnthropicTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum AnthropicToolChoice {
    Auto,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum AnthropicThinkingParam {
    Enabled { budget_tokens: u32 },
    Adaptive { display: String },
}

/// Build the full Anthropic Messages request body. `tool_choice: None`
/// is sent when no tools are present.
pub(super) fn build_messages_request(
    model: &str,
    messages: &[Message],
    tools: &[ToolSchema],
    stream: bool,
    options: Option<AnthropicThinking>,
) -> AnthropicMessagesRequest {
    let (system, messages) = to_anthropic_messages(messages);
    let tools = to_anthropic_tools(tools);
    let has_tools = !tools.is_empty();
    let manual_thinking = matches!(options, Some(AnthropicThinking::Manual { .. }));
    let output_config = build_output_config(&options);

    AnthropicMessagesRequest {
        model: model.to_string(),
        system,
        messages,
        tools,
        tool_choice: if has_tools {
            Some(AnthropicToolChoice::Auto)
        } else {
            None
        },
        max_tokens: 16_384,
        stream,
        thinking: build_thinking_param(options),
        temperature: if manual_thinking { Some(1.0) } else { None },
        output_config,
    }
}

/// Translate `ToolSchema`s to the Anthropic tool definition format.
fn to_anthropic_tools(tools: &[ToolSchema]) -> Vec<AnthropicTool> {
    tools
        .iter()
        .map(|t| AnthropicTool {
            name: t.name.clone(),
            description: t.description.clone(),
            input_schema: t.parameters.clone(),
        })
        .collect()
}

/// Translate `Message`s to the Anthropic wire format. Tool-result
/// messages get flushed into the *previous* user message because the
/// API expects tool results to live inside a user-role turn, not as
/// their own turn.
pub(super) fn to_anthropic_messages(messages: &[Message]) -> (Option<String>, Vec<AnthropicMessage>) {
    let mut system = None;
    let mut wire = Vec::new();
    let mut pending_tool_results: Vec<AnthropicContentBlock> = Vec::new();

    for message in messages {
        if !pending_tool_results.is_empty() && message.role != Role::Tool {
            flush_tool_results(&mut wire, &mut pending_tool_results);
        }

        match message.role {
            Role::System => {
                system = Some(content_to_text(&message.content));
            }
            Role::User => {
                wire.push(AnthropicMessage {
                    role: "user".into(),
                    content: content_to_wire_user(&message.content),
                });
            }
            Role::Assistant => {
                let mut blocks = Vec::new();
                let text = content_to_text(&message.content);
                if !text.is_empty() {
                    blocks.push(AnthropicContentBlock::Text { text });
                }
                if let Some(calls) = &message.tool_calls {
                    for call in calls {
                        blocks.push(AnthropicContentBlock::ToolUse {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            input: call.arguments.clone(),
                        });
                    }
                }

                wire.push(AnthropicMessage {
                    role: "assistant".into(),
                    content: AnthropicMessageContent::Blocks(blocks),
                });
            }
            Role::Tool => {
                pending_tool_results.push(AnthropicContentBlock::ToolResult {
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

/// Merge pending tool-result blocks into the previous user message
/// (or push a new user message if there is no previous one). Anthropic
/// requires tool results to be inside a user-role turn.
fn flush_tool_results(
    wire: &mut Vec<AnthropicMessage>,
    pending: &mut Vec<AnthropicContentBlock>,
) {
    if pending.is_empty() {
        return;
    }

    let results = std::mem::take(pending);
    if let Some(last) = wire.last_mut() {
        if last.role == "user" {
            match &mut last.content {
                AnthropicMessageContent::Text(text) => {
                    let mut blocks = vec![AnthropicContentBlock::Text {
                        text: std::mem::take(text),
                    }];
                    blocks.extend(results);
                    last.content = AnthropicMessageContent::Blocks(blocks);
                }
                AnthropicMessageContent::Blocks(blocks) => blocks.extend(results),
            }
            return;
        }
    }

    wire.push(AnthropicMessage {
        role: "user".into(),
        content: AnthropicMessageContent::Blocks(results),
    });
}

fn content_to_wire_user(content: &Content) -> AnthropicMessageContent {
    AnthropicMessageContent::Text(content_to_text(content))
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

fn build_thinking_param(thinking: Option<AnthropicThinking>) -> Option<AnthropicThinkingParam> {
    match thinking {
        Some(AnthropicThinking::Manual { budget_tokens }) => {
            Some(AnthropicThinkingParam::Enabled { budget_tokens })
        }
        Some(AnthropicThinking::Adaptive { display, effort: _ }) => {
            Some(AnthropicThinkingParam::Adaptive { display })
        }
        None => None,
    }
}

fn build_output_config(thinking: &Option<AnthropicThinking>) -> Option<AnthropicOutputConfig> {
    match thinking {
        Some(AnthropicThinking::Adaptive {
            effort: Some(effort),
            ..
        }) => Some(AnthropicOutputConfig {
            effort: effort.clone(),
        }),
        _ => None,
    }
}
