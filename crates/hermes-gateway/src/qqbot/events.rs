//! Conversion from `qq_bot_rs` events to the gateway's `GatewayEvent`,
//! plus the [`QqEventHandler`] that streams agent output to QQ chats.

use std::sync::Arc;

use perry_hermes_core::Platform;
use perry_hermes_core::message::ToolCall;
use qq_bot_rs::types::message::{C2cMessage, GroupMessage, OutgoingMessage};
use qq_bot_rs::types::payloads::MarkdownPayload;

use crate::event::{ChatType, GatewayEvent};
use crate::handler::GatewayEventHandler;
use crate::runner::GatewayRunner;

// ── Event conversion ────────────────────────────────────────────────

/// Strip `<@!botId> ` mention prefix from group message content.
///
/// QQ's protocol embeds the bot mention at the start of group @ messages.
/// We strip the prefix so the LLM sees clean text.
fn strip_at_mention(content: &str) -> &str {
    if let Some(rest) = content.strip_prefix("<@!")
        && let Some(space_idx) = rest.find(' ')
    {
        return &rest[space_idx + 1..];
    }
    content
}

/// Parse QQ's ISO 8601 timestamp string into `chrono::DateTime<Utc>`.
///
/// Returns `Utc::now()` on parse failure (the message is still routed; the
/// timestamp is metadata only).
fn parse_qq_timestamp(s: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now())
}

/// Convert a `C2cMessage` to a `GatewayEvent`.
///
/// Returns `None` if the message has no text (e.g. attachment-only).
pub fn c2c_to_event(msg: &C2cMessage) -> Option<GatewayEvent> {
    let text = msg.content.trim();
    if text.is_empty() {
        return None;
    }
    Some(GatewayEvent {
        platform: Platform::QqBot,
        chat_id: msg.author.user_openid.clone(),
        chat_type: ChatType::Dm,
        user_id: msg.author.user_openid.clone(),
        user_name: None,
        thread_id: None,
        text: text.to_string(),
        message_id: Some(msg.id.clone()),
        timestamp: parse_qq_timestamp(&msg.timestamp),
    })
}

/// Convert a `GroupMessage` to a `GatewayEvent`.
///
/// Strips the leading `<@!botId> ` mention and returns `None` if the
/// remaining text is empty.
pub fn group_to_event(msg: &GroupMessage) -> Option<GatewayEvent> {
    let text = strip_at_mention(&msg.content).trim();
    if text.is_empty() {
        return None;
    }
    Some(GatewayEvent {
        platform: Platform::QqBot,
        chat_id: msg.group_openid.clone(),
        chat_type: ChatType::Group,
        user_id: msg.author.member_openid.clone(),
        user_name: None,
        thread_id: None,
        text: text.to_string(),
        message_id: Some(msg.id.clone()),
        timestamp: parse_qq_timestamp(&msg.timestamp),
    })
}

// ── Streaming event handler ─────────────────────────────────────────

/// Whether the target is a C2C (direct) or group chat.
#[derive(Debug, Clone, Copy)]
enum QqTarget {
    C2c,
    Group,
}

/// Streams agent output to a QQ chat, sending each content segment as
/// a separate message.
///
/// Content is buffered in `content_buffer` and flushed at tool call
/// boundaries (`on_tool_started`) and turn completion
/// (`on_turn_completed`). Each flush sends one QQ message.
pub struct QqEventHandler {
    bot: Arc<qq_bot_rs::Bot>,
    target_id: String,
    target: QqTarget,
    content_buffer: String,
}

impl QqEventHandler {
    pub fn new_c2c(bot: Arc<qq_bot_rs::Bot>, user_openid: String) -> Self {
        Self {
            bot,
            target_id: user_openid,
            target: QqTarget::C2c,
            content_buffer: String::new(),
        }
    }

    pub fn new_group(bot: Arc<qq_bot_rs::Bot>, group_openid: String) -> Self {
        Self {
            bot,
            target_id: group_openid,
            target: QqTarget::Group,
            content_buffer: String::new(),
        }
    }

    /// Flush accumulated content as a QQ message. No-op if buffer is empty.
    fn flush(&mut self) {
        let text = std::mem::take(&mut self.content_buffer);
        if text.trim().is_empty() {
            return;
        }
        let bot = Arc::clone(&self.bot);
        let target_id = self.target_id.clone();
        let target = self.target;
        tokio::spawn(async move {
            let reply = OutgoingMessage::markdown(MarkdownPayload::raw(&text));
            let result = match target {
                QqTarget::C2c => bot.post_c2c_message(&target_id, &reply).await,
                QqTarget::Group => bot.post_group_message(&target_id, &reply).await,
            };
            if let Err(e) = result {
                tracing::warn!(error = %e, "qqbot: send failed");
            }
        });
    }
}

impl GatewayEventHandler for QqEventHandler {
    fn on_content_delta(&mut self, text: &str) {
        self.content_buffer.push_str(text);
    }

    fn on_tool_started(&mut self, _call: &ToolCall, _iteration: u32) {
        // Flush content accumulated before this tool call.
        self.flush();
    }

    fn on_error(&mut self, error: &str) {
        self.content_buffer.push_str(&format!("⚠ Error: {error}"));
    }

    fn on_turn_completed(&mut self) {
        // Flush any remaining content from the final iteration.
        self.flush();
    }
}

// ── Handler dispatch ────────────────────────────────────────────────

/// Run a single `GatewayEvent` through the gateway, streaming agent
/// output to the QQ chat via [`QqEventHandler`].
pub async fn handle_reply(
    gateway: &GatewayRunner,
    event: &GatewayEvent,
    handler: &mut QqEventHandler,
) {
    match gateway.handle_event(event.clone(), handler).await {
        Ok(crate::runner::GatewayResponse::CommandReply(text)) => {
            let bot = Arc::clone(&handler.bot);
            let target_id = handler.target_id.clone();
            let target = handler.target;
            tokio::spawn(async move {
                let reply = OutgoingMessage::markdown(MarkdownPayload::raw(&text));
                let result = match target {
                    QqTarget::C2c => bot.post_c2c_message(&target_id, &reply).await,
                    QqTarget::Group => bot.post_group_message(&target_id, &reply).await,
                };
                if let Err(e) = result {
                    tracing::warn!(error = %e, "qqbot: send command reply failed");
                }
            });
        }
        Ok(crate::runner::GatewayResponse::Ignored) => {}
        Err(e) => {
            tracing::warn!(error = %e, "qqbot: gateway error");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_at_mention_with_mention() {
        assert_eq!(strip_at_mention("<@!12345> hello world"), "hello world");
    }

    #[test]
    fn strip_at_mention_without_mention() {
        assert_eq!(strip_at_mention("no mention here"), "no mention here");
    }

    #[test]
    fn strip_at_mention_empty() {
        assert_eq!(strip_at_mention(""), "");
    }

    #[test]
    fn strip_at_mention_partial_prefix_only() {
        // No space after the bot id — leave unchanged.
        assert_eq!(strip_at_mention("<@!12345>"), "<@!12345>");
    }

    use serde_json::json;

    #[test]
    fn c2c_to_event_maps_fields() {
        let msg: C2cMessage = serde_json::from_value(json!({
            "id": "MSG_ID_1",
            "author": { "user_openid": "U_OPENID_1" },
            "content": "hello there",
            "attachments": [],
            "timestamp": "2026-06-09T08:00:00+00:00"
        }))
        .unwrap();
        let ev = c2c_to_event(&msg).expect("event");
        assert_eq!(ev.platform, Platform::QqBot);
        assert_eq!(ev.chat_id, "U_OPENID_1");
        assert_eq!(ev.user_id, "U_OPENID_1");
        assert!(matches!(ev.chat_type, ChatType::Dm));
        assert_eq!(ev.text, "hello there");
        assert_eq!(ev.message_id.as_deref(), Some("MSG_ID_1"));
    }

    #[test]
    fn c2c_to_event_returns_none_for_empty_text() {
        let msg: C2cMessage = serde_json::from_value(json!({
            "id": "X",
            "author": { "user_openid": "U" },
            "content": "   ",
            "attachments": [],
            "timestamp": "2026-06-09T08:00:00+00:00"
        }))
        .unwrap();
        assert!(c2c_to_event(&msg).is_none());
    }

    #[test]
    fn group_to_event_maps_fields_and_strips_mention() {
        let msg: GroupMessage = serde_json::from_value(json!({
            "id": "G_MSG_1",
            "group_openid": "G_OPENID_1",
            "author": { "member_openid": "M_OPENID_1" },
            "content": "<@!BOTID> ping the bot",
            "attachments": [],
            "timestamp": "2026-06-09T08:00:00+00:00"
        }))
        .unwrap();
        let ev = group_to_event(&msg).expect("event");
        assert_eq!(ev.chat_id, "G_OPENID_1");
        assert_eq!(ev.user_id, "M_OPENID_1");
        assert!(matches!(ev.chat_type, ChatType::Group));
        assert_eq!(ev.text, "ping the bot");
        assert_eq!(ev.message_id.as_deref(), Some("G_MSG_1"));
    }

    #[test]
    fn group_to_event_returns_none_when_only_mention() {
        let msg: GroupMessage = serde_json::from_value(json!({
            "id": "G_MSG_2",
            "group_openid": "G",
            "author": { "member_openid": "M" },
            "content": "<@!BOTID> ",
            "attachments": [],
            "timestamp": "2026-06-09T08:00:00+00:00"
        }))
        .unwrap();
        assert!(group_to_event(&msg).is_none());
    }
}
