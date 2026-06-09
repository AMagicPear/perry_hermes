use chrono::{DateTime, Utc};

use perry_hermes_core::Platform;

/// Normalized incoming message from any platform adapter.
///
/// This is the gateway's equivalent of Python's `MessageEvent` — a
/// platform-agnostic representation of an incoming user message that
/// the gateway can process without knowing the source platform.
#[derive(Debug, Clone)]
pub struct GatewayEvent {
    /// Source platform. Used as the leading segment of the session key
    /// (see [`crate::runner::build_key`]) and as the namespace for
    /// per-platform configuration like `allowed_users`.
    pub platform: Platform,
    /// Platform-specific chat identifier.
    pub chat_id: String,
    /// Type of chat this message came from.
    pub chat_type: ChatType,
    /// Platform-specific user identifier.
    pub user_id: String,
    /// Display name of the sender (best-effort).
    pub user_name: Option<String>,
    /// Thread / forum topic identifier, if applicable.
    pub thread_id: Option<String>,
    /// The user's message text.
    pub text: String,
    /// Platform-specific message ID (for reply threading).
    pub message_id: Option<String>,
    /// When the message was sent.
    pub timestamp: DateTime<Utc>,
}

/// Type of chat a message originated from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatType {
    /// Direct / private message.
    Dm,
    /// Group chat.
    Group,
    /// Channel (broadcast).
    Channel,
    /// Thread / forum topic within a group or channel.
    Thread,
}

impl ChatType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChatType::Dm => "dm",
            ChatType::Group => "group",
            ChatType::Channel => "channel",
            ChatType::Thread => "thread",
        }
    }
}

impl std::fmt::Display for ChatType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
