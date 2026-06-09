//! Conversion from `qq_bot_rs` events to the gateway's `GatewayEvent`.

#![allow(dead_code, unused_imports)] // populated incrementally by Tasks 7 + 8

use qq_bot_rs::types::message::{C2cMessage, GroupMessage};

use crate::event::{ChatType, GatewayEvent};
use crate::runner::GatewayRunner;

/// Strip `<@!botId> ` mention prefix from group message content.
///
/// QQ's protocol embeds the bot mention at the start of group @ messages.
/// We strip the prefix so the LLM sees clean text.
fn strip_at_mention(content: &str) -> &str {
    if let Some(rest) = content.strip_prefix("<@!") {
        if let Some(space_idx) = rest.find(' ') {
            return &rest[space_idx + 1..];
        }
    }
    content
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
}
