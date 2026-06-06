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
    /// Convenience constructor for a plain user-role text message.
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Content::Text(text.into()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// Convenience constructor for a plain assistant-role text message.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: Content::Text(text.into()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// Convenience constructor for a system-role text message.
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: Content::Text(text.into()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// Convenience constructor for a tool-role result message paired with
    /// the call id from the assistant's `tool_calls`.
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
    pub fn char_len(&self) -> usize {
        let content_chars = self.content.chars();
        let reasoning_chars = self.reasoning.as_ref().map_or(0, |s| s.len());
        let tool_calls_chars: usize = self.tool_calls.as_ref().map_or(0, |calls| {
            calls.iter().map(|c| c.arguments.to_string().len()).sum()
        });
        content_chars + reasoning_chars + tool_calls_chars
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
    /// Total character count across all text/image_url parts.
    pub fn chars(&self) -> usize {
        match self {
            Content::Text(s) => s.len(),
            Content::Parts(parts) => parts
                .iter()
                .map(|p| match p {
                    ContentPart::Text { text } => text.len(),
                    ContentPart::ImageUrl { url } => url.len(),
                })
                .sum(),
        }
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
