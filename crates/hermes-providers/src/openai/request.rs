//! Request DTOs and the `build_chat_request` builder for the OpenAI
//! Chat Completions endpoint.
//!
//! The DTO names are prefixed `Openai*` (not `Oai*`) so they match the
//! brand and read as "the OpenAI thing" at a glance. The `to_openai_*`
//! free functions translate from `hermes_core` types.

use serde::Serialize;

use hermes_core::message::{Content, ContentPart, Message};
use hermes_core::registry::ToolSchema;

/// The full Chat Completions request body, serialized as JSON.
#[derive(Serialize)]
pub(super) struct OpenaiChatRequest<'a> {
    pub model: &'a str,
    pub messages: Vec<OpenaiMessage<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<OpenaiTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<&'static str>,
    pub stream: bool,
    /// Ask OpenAI to include token usage in the stream's final chunk
    /// (otherwise `in`/`out` metrics stay at 0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<OpenaiStreamOptions>,
}

#[derive(Serialize)]
pub(super) struct OpenaiStreamOptions {
    pub include_usage: bool,
}

#[derive(Serialize)]
pub(super) struct OpenaiMessage<'a> {
    pub role: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<OpenaiMessageContent<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<&'a str>,
    /// Round-trip the LLM's `tool_calls` so the next request remembers
    /// which tools it invoked. OpenAI expects each entry as
    /// `{ id, type: "function", function: { name, arguments } }`.
    /// `arguments` is sent as a JSON *string* (matching how OpenAI
    /// returns it), not a nested object.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenaiToolCallRef<'a>>>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum OpenaiMessageContent<'a> {
    Text(&'a str),
    Parts(Vec<OpenaiContentPart<'a>>),
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum OpenaiContentPart<'a> {
    Text { text: &'a str },
    ImageUrl { image_url: OpenaiImageUrl<'a> },
}

#[derive(Serialize)]
pub(super) struct OpenaiImageUrl<'a> {
    pub url: &'a str,
}

#[derive(Serialize)]
pub(super) struct OpenaiToolCallRef<'a> {
    pub id: &'a str,
    pub r#type: &'static str, // "function"
    pub function: OpenaiFunctionCallRef<'a>,
}

#[derive(Serialize)]
pub(super) struct OpenaiFunctionCallRef<'a> {
    pub name: &'a str,
    /// JSON-stringified arguments, matching how OpenAI returns them.
    pub arguments: String,
}

#[derive(Serialize)]
pub(super) struct OpenaiTool<'a> {
    pub r#type: &'static str,
    pub function: OpenaiFunctionDef<'a>,
}

#[derive(Serialize)]
pub(super) struct OpenaiFunctionDef<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub parameters: &'a serde_json::Value,
}

/// Convert a single `Message` into the OpenAI wire format. Tool-call
/// arguments are serialized to JSON strings (OpenAI's required shape).
pub(super) fn to_openai_message(m: &Message) -> OpenaiMessage<'_> {
    let tool_calls = m.tool_calls.as_ref().map(|calls| {
        calls
            .iter()
            .map(|c| OpenaiToolCallRef {
                id: &c.id,
                r#type: "function",
                function: OpenaiFunctionCallRef {
                    name: &c.name,
                    arguments: serde_json::to_string(&c.arguments)
                        .unwrap_or_else(|_| "null".into()),
                },
            })
            .collect()
    });
    OpenaiMessage {
        role: m.role.as_str(),
        content: match &m.content {
            Content::Text(s) => Some(OpenaiMessageContent::Text(s.as_str())),
            Content::Parts(parts) => Some(OpenaiMessageContent::Parts(
                parts
                    .iter()
                    .map(|p| match p {
                        ContentPart::Text { text } => OpenaiContentPart::Text {
                            text: text.as_str(),
                        },
                        ContentPart::ImageUrl { url } => OpenaiContentPart::ImageUrl {
                            image_url: OpenaiImageUrl { url: url.as_str() },
                        },
                    })
                    .collect(),
            )),
        },
        tool_call_id: m.tool_call_id.as_deref(),
        tool_calls,
    }
}

/// Convert a single `ToolSchema` into the OpenAI tool-call wire format.
pub(super) fn to_openai_tool(t: &ToolSchema) -> OpenaiTool<'_> {
    OpenaiTool {
        r#type: "function",
        function: OpenaiFunctionDef {
            name: &t.name,
            description: &t.description,
            parameters: &t.parameters,
        },
    }
}

/// Build the full Chat Completions request body. `tool_choice: None` is
/// sent when no tools are present because some OpenAI-compatible
/// providers reject `tool_choice: "auto"` with an empty tool list.
pub(super) fn build_chat_request<'a>(
    model: &'a str,
    messages: &'a [Message],
    tools: &'a [ToolSchema],
) -> OpenaiChatRequest<'a> {
    let oai_msgs: Vec<OpenaiMessage<'a>> = messages.iter().map(to_openai_message).collect();
    let oai_tools: Vec<OpenaiTool<'a>> = tools.iter().map(to_openai_tool).collect();
    let has_tools = !oai_tools.is_empty();
    OpenaiChatRequest {
        model,
        messages: oai_msgs,
        tools: oai_tools,
        tool_choice: if has_tools { Some("auto") } else { None },
        stream: true,
        stream_options: Some(OpenaiStreamOptions {
            include_usage: true,
        }),
    }
}
