//! Conversation message types shared between providers, the loop, and tools.

use serde::{Deserialize, Serialize};

/// A single message in a conversation.
///
/// The `reasoning` field carries chain-of-thought content for providers that
/// support it (Anthropic extended thinking, OpenAI o1/o3, etc.). It lives on
/// the message — not in a separate field — because reasoning is part of the
/// assistant message and must travel with it through compression,
/// serialization, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Content,

    /// Some providers put reasoning here. Optional so plain messages don't
    /// pay the cost.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,

    /// Tool call id round-trip — OpenAI uses `tool_call_id`, Anthropic uses
    /// `tool_use_id`. Normalize at the provider adapter layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl Message {
    /// Build a user-role text message. Use for any new turn from the
    /// human; tool-call responses go through `Message::tool_result`.
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Content::Text(text.into()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// Build an assistant-role text message. Use for the final reply in a
    /// turn that has no tool calls; turns with tool calls carry a full
    /// `Message` (with `tool_calls`) from the provider's `Completion`.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: Content::Text(text.into()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// Build a system-role text message. `AgentLoop` prepends one of these
    /// to the conversation if the loop's `LoopConfig.system_prompt` is set
    /// and the caller hasn't supplied their own.
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: Content::Text(text.into()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// Build a tool-role result message paired with the `id` of the
    /// assistant's `tool_call` that produced it. Required by every
    /// provider's tool-result schema (Anthropic `tool_use_id`,
    /// OpenAI `tool_call_id`).
    pub fn tool_result(tool_call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: Content::Text(text.into()),
            reasoning: None,
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: None,
        }
    }

    /// Total character count across content, reasoning, and tool-call args.
    /// Used by the compressor to estimate tokens without a tokenizer.
    /// Allocates a `String` per tool call to format the JSON arguments;
    /// for hot loops, prefer `char_len_into` with a reused buffer.
    pub fn char_len(&self) -> usize {
        let mut buf = String::new();
        self.char_len_into(&mut buf)
    }

    /// Like `char_len`, but reuses `buf` for tool-call JSON serialization
    /// to avoid per-call allocation. The buffer is cleared and refilled
    /// during the call; its final content is unspecified.
    pub fn char_len_into(&self, buf: &mut String) -> usize {
        let mut total = self.content.chars();
        total += self.reasoning.as_ref().map_or(0, |s| s.chars().count());
        if let Some(calls) = &self.tool_calls {
            for call in calls {
                buf.clear();
                total += serde_json::to_string(&call.arguments)
                    .unwrap_or_default()
                    .chars()
                    .count();
            }
        }
        total
    }
}

/// Who produced a message.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// Message body. Untagged so the same field accepts both `"hello"` (string)
/// and `[{"type": "text", ...}, ...]` (multimodal array). LLM APIs accept
/// both shapes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl Content {
    /// Total character count across all text/image_url parts. Used by the
    /// compressor and the context-usage estimator to size up a message
    /// without a tokenizer.
    pub fn chars(&self) -> usize {
        match self {
            Content::Text(s) => s.chars().count(),
            Content::Parts(parts) => parts.iter().map(ContentPart::chars).sum(),
        }
    }

    /// `true` when this content is a single text part (no images, no
    /// multimodal structure). Lets callers short-circuit without a `match`.
    pub fn is_text(&self) -> bool {
        matches!(self, Content::Text(_))
    }

    /// Concatenated text across all text parts. Image parts contribute an
    /// `[image: <url>]` marker so summaries are still meaningful.
    pub fn as_text(&self) -> String {
        match self {
            Content::Text(s) => s.clone(),
            Content::Parts(parts) => parts
                .iter()
                .map(|p| match p {
                    ContentPart::Text { text } => text.clone(),
                    ContentPart::ImageUrl { url } => format!("[image: {url}]"),
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

/// A single part of a multimodal message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { url: String },
    // future: Audio, File, ...
}

impl ContentPart {
    /// Character count contributed by this part. Image URLs count their
    /// string length; future media parts should report a useful proxy
    /// (e.g. file size, duration) so token estimates stay roughly right.
    pub fn chars(&self) -> usize {
        match self {
            ContentPart::Text { text } => text.chars().count(),
            ContentPart::ImageUrl { url } => url.chars().count(),
        }
    }
}

/// A tool call the LLM wants to make.
///
/// `arguments` is `serde_json::Value`, not a typed struct. Strong typing
/// here would force core to know every tool's schema; the schema is per-tool
/// and validated at tool-dispatch time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON. Don't parse into a typed struct at the core layer —
    /// parsing is each tool's responsibility, since the schema is per-tool.
    pub arguments: serde_json::Value,
}

impl ToolCall {
    /// Build a new tool call. `id` and `name` are owned so they travel
    /// through serialization without lifetime tracking; `arguments` is
    /// the already-parsed JSON value the LLM streamed.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_serializes_lowercase() {
        let s = serde_json::to_string(&Role::Tool).unwrap();
        assert_eq!(s, "\"tool\"");
    }

    #[test]
    fn content_untagged_accepts_string() {
        let c: Content = serde_json::from_str("\"hi\"").unwrap();
        assert!(matches!(c, Content::Text(ref t) if t == "hi"));
    }

    #[test]
    fn content_untagged_accepts_array() {
        let c: Content = serde_json::from_str(r#"[{"type":"text","text":"hi"}]"#).unwrap();
        assert!(matches!(c, Content::Parts(ref parts) if parts.len() == 1));
    }

    #[test]
    fn message_skips_none_fields() {
        let m = Message {
            role: Role::User,
            content: Content::Text("hi".into()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        };
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(s, r#"{"role":"user","content":"hi"}"#);
    }
}
