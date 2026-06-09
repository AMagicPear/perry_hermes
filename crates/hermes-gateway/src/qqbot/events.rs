//! Conversion from `qq_bot_rs` events to the gateway's `GatewayEvent`.

use qq_bot_rs::types::message::{C2cMessage, GroupMessage};

use crate::event::{ChatType, GatewayEvent};
use crate::runner::GatewayRunner;

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
        platform: "qqbot".into(),
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
        platform: "qqbot".into(),
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

/// Run a single `GatewayEvent` through the gateway and ship the reply
/// back via the provided async `send` closure.
///
/// Failures are logged via `tracing`; the bridge does not retry.
pub async fn handle_reply<F, Fut>(gateway: &GatewayRunner, event: &GatewayEvent, send: F)
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    match gateway.handle_event(event.clone()).await {
        Ok(crate::runner::GatewayResponse::Reply(text)) => {
            if let Err(e) = send(text).await {
                tracing::warn!(error = %e, "qqbot: send reply failed");
            }
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
        assert_eq!(ev.platform, "qqbot");
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
