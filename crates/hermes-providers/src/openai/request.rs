//! Request DTOs and the `build_chat_request` builder for the OpenAI
//! Chat Completions endpoint.
//!
//! The DTO names are prefixed `Openai*` (not `Oai*`) so they match the
//! brand and read as "the OpenAI thing" at a glance. The `to_openai_*`
//! free functions translate from `perry_hermes_core` types.

use serde::Serialize;

use perry_hermes_core::message::{Content, ContentPart, Message};
use perry_hermes_core::registry::ToolSchema;

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
    /// Round-trip the LLM's chain-of-thought on subsequent turns. Reasoning
    /// models (DeepSeek-R1, Doubao 1.5 pro thinking, etc.) reject requests
    /// where a prior assistant turn had reasoning but no visible content,
    /// because the wire message ends up with empty `content` and no
    /// `reasoning_content` — exactly the failure case
    /// `assistant must provide content, reasoningcontent or toolcalls`.
    /// Carrying the reasoning back keeps the conversation coherent for
    /// the model on the next turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<&'a str>,
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

/// `true` when an assistant message has nothing meaningful to send: no
/// visible content, no chain-of-thought, and no tool calls. Providers
/// (Doubao, DeepSeek) reject this with
/// `assistant must provide content, reasoning_content or tool_calls`.
/// Filter it out of the request so a prior "thought-only" turn cannot
/// break the next call.
pub(super) fn assistant_is_empty(m: &Message) -> bool {
    if m.role != perry_hermes_core::message::Role::Assistant {
        return false;
    }
    let content_empty = match &m.content {
        Content::Text(s) => s.is_empty(),
        Content::Parts(parts) => parts.is_empty(),
    };
    let reasoning_empty = m.reasoning.as_deref().is_none_or(str::is_empty);
    let tool_calls_empty = m.tool_calls.as_ref().is_none_or(Vec::is_empty);
    content_empty && reasoning_empty && tool_calls_empty
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
    // For an assistant message with no visible text, drop the `content`
    // field entirely instead of sending an empty string. The provider
    // treats `content: ""` as "the assistant said nothing" and still
    // requires at least one of content / reasoning_content / tool_calls.
    // Omitting the key avoids the trap when the LLM produced only a
    // tool call (where `content` is naturally empty) — `reasoning_content`
    // and `tool_calls` then satisfy the requirement.
    let raw_content = match &m.content {
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
    };
    let content = match (&raw_content, m.role.as_str()) {
        (Some(OpenaiMessageContent::Text("")), "assistant") => None,
        _ => raw_content,
    };
    OpenaiMessage {
        role: m.role.as_str(),
        content,
        reasoning_content: m.reasoning.as_deref(),
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
///
/// Empty assistant messages (no content, no reasoning, no tool calls)
/// are dropped before serialization. They cannot be sent to providers
/// that require at least one of those fields, and they have no
/// information worth keeping on the wire — the LLM produced nothing
/// visible and we have already shown the user whatever reasoning we
/// want them to see locally.
pub(super) fn build_chat_request<'a>(
    model: &'a str,
    messages: &'a [Message],
    tools: &'a [ToolSchema],
) -> OpenaiChatRequest<'a> {
    let oai_msgs: Vec<OpenaiMessage<'a>> = messages
        .iter()
        .filter(|m| !assistant_is_empty(m))
        .map(to_openai_message)
        .collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use perry_hermes_core::message::{Content, Message, Role, ToolCall};
    use serde_json::json;

    fn assistant_with(
        content: &str,
        reasoning: Option<&str>,
        tool_calls: Vec<ToolCall>,
    ) -> Message {
        Message {
            role: Role::Assistant,
            content: Content::Text(content.into()),
            reasoning: reasoning.map(str::to_string),
            tool_call_id: None,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
        }
    }

    fn user_with(content: &str) -> Message {
        Message {
            role: Role::User,
            content: Content::Text(content.into()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    #[test]
    fn assistant_is_empty_true_when_no_content_reasoning_or_tool_calls() {
        // Reproduces the bug: LLM produced chain-of-thought but no
        // visible reply, the message was persisted with `content: ""`
        // and `reasoning: "..."` and no tool calls. The provider then
        // rejected with `assistant must provide content, reasoningcontent
        // or toolcalls`.
        let m = assistant_with("", Some("你来了一个，我回你一个 😂 去休息啦"), vec![]);
        // The reasoning makes this *not* empty for transport — the
        // wire format will carry it as `reasoning_content`.
        assert!(!assistant_is_empty(&m));

        // The fully empty case (no reasoning either) is the real
        // trap; this one must be filtered out at the request layer.
        let m = assistant_with("", None, vec![]);
        assert!(assistant_is_empty(&m));
    }

    #[test]
    fn assistant_is_empty_false_for_non_assistant_roles() {
        // A user message with empty content must not be filtered —
        // that would be a different bug. The guard is role-scoped.
        let m = user_with("");
        assert!(!assistant_is_empty(&m));
    }

    #[test]
    fn assistant_is_empty_false_when_tool_calls_present() {
        let call = ToolCall::new("call_1", "terminal", json!({"command": "ls"}));
        let m = assistant_with("", None, vec![call]);
        assert!(!assistant_is_empty(&m));
    }

    #[test]
    fn reasoning_is_translated_to_reasoning_content_on_wire() {
        // Regression: previously the local `reasoning` field was not
        // mapped to the provider's `reasoning_content` field, so a
        // thought-only assistant turn became an empty assistant
        // message on the wire and the provider rejected the next
        // request with `assistant must provide content, reasoningcontent
        // or toolcalls`.
        let m = assistant_with("", Some("thinking..."), vec![]);
        let wire = to_openai_message(&m);
        assert_eq!(wire.reasoning_content, Some("thinking..."));
        // Empty assistant content must be dropped, not sent as `""`.
        assert!(wire.content.is_none());
        assert!(wire.tool_calls.is_none());
    }

    #[test]
    fn non_empty_assistant_content_is_preserved() {
        let m = assistant_with("hello", Some("thinking..."), vec![]);
        let wire = to_openai_message(&m);
        assert!(matches!(
            wire.content,
            Some(OpenaiMessageContent::Text("hello"))
        ));
        assert_eq!(wire.reasoning_content, Some("thinking..."));
    }

    #[test]
    fn user_message_content_is_never_dropped() {
        // A user message with an empty body is unusual but legal
        // (e.g. an image-only turn). The empty-content filter is
        // assistant-only; user messages must round-trip as-is so we
        // do not silently erase user input.
        let m = user_with("");
        let wire = to_openai_message(&m);
        assert!(matches!(wire.content, Some(OpenaiMessageContent::Text(""))));
    }

    #[test]
    fn build_chat_request_drops_empty_assistant_messages() {
        // Reproduces the user-reported bug end-to-end. The prior
        // assistant turn produced only reasoning (no content, no
        // tool calls); the next turn's request must drop the empty
        // message entirely so the provider does not reject with
        // `assistant must provide content, reasoningcontent or toolcalls`.
        let messages = vec![
            user_with("hi"),
            assistant_with("hello!", None, vec![]),
            user_with("how are you?"),
            // The bad message from the bug report.
            assistant_with("", Some("just thinking"), vec![]),
            // And the fully empty variant that cannot be salvaged.
            assistant_with("", None, vec![]),
        ];
        let req = build_chat_request("model", &messages, &[]);
        // The two kept assistant messages plus two user messages = 4.
        assert_eq!(req.messages.len(), 4);
        let roles: Vec<&str> = req.messages.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec!["user", "assistant", "user", "assistant"]);

        // Verify the thought-only-but-with-reasoning message survived
        // and carries `reasoning_content` on the wire.
        let last_assistant = req.messages.last().unwrap();
        assert_eq!(last_assistant.reasoning_content, Some("just thinking"));
        assert!(last_assistant.content.is_none());
    }
}
