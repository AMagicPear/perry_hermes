//! Cheap pre-passes that reduce token count without an LLM call.
//!
//! Two pruning strategies:
//! 1. **Old tool-result pruning** — replace tool results in the middle slice
//!    with one-line summaries. No LLM call.
//! 2. **Tail trimming** — walk backwards from the end, accumulating tokens
//!    up to the configured tail budget.

use hermes_core::message::{Content, Message, Role};

use super::compressor::CompressorConfig;

/// Replace old tool-result contents with one-line summaries.
///
/// Iterates oldest → newest, skipping the last `protect_tail_tokens` worth
/// of messages. Tool results older than the tail become
/// `"[Old tool output cleared, {tool_name} returned {N} bytes]"`.
///
/// Returns `(new_messages, total_chars_pruned)`.
pub fn prune_old_tool_results(
    messages: &[Message],
    config: &CompressorConfig,
) -> (Vec<Message>, usize) {
    if messages.is_empty() {
        return (messages.to_vec(), 0);
    }

    // Walk backwards to find how many messages are in the tail (protected).
    let tail_budget = config.protect_tail_tokens;
    let mut tail_chars_remaining = (tail_budget as f64 * 4.0) as i64;
    let mut tail_start = messages.len();

    for i in (0..messages.len()).rev() {
        let msg_chars = estimate_message_chars(&messages[i]) as i64;
        tail_chars_remaining -= msg_chars;
        if tail_chars_remaining <= 0 {
            tail_start = i;
            break;
        }
    }

    let mut new_messages = Vec::with_capacity(messages.len());
    let mut total_pruned = 0usize;

    for (i, msg) in messages.iter().enumerate() {
        if i >= tail_start {
            // This message is in the protected tail — keep as-is.
            new_messages.push(msg.clone());
            continue;
        }

        if msg.role == Role::Tool {
            // Prune: replace the tool result content with a one-line summary.
            let original_chars = estimate_message_chars(msg);
            let tool_name = msg
                .tool_call_id
                .as_deref()
                .unwrap_or("unknown")
                .to_string();
            let pruned_text = format!(
                "[Old tool output cleared, {} returned {} bytes]",
                tool_name, original_chars
            );
            let pruned_chars = pruned_text.len();
            total_pruned += original_chars.saturating_sub(pruned_chars);
            new_messages.push(Message {
                role: Role::Tool,
                content: Content::Text(pruned_text),
                reasoning: None,
                tool_call_id: msg.tool_call_id.clone(),
                tool_calls: None,
            });
        } else {
            // Keep non-tool messages in the head/middle as-is.
            new_messages.push(msg.clone());
        }
    }

    (new_messages, total_pruned)
}

/// Find the index after the first `protect_first_n` non-system messages.
///
/// The system prompt (index 0) is always preserved. This returns the index
/// of the first message that is NOT in the protected head.
pub fn find_head_boundary(messages: &[Message], protect_first_n: usize) -> usize {
    let mut count = 0usize;
    for (i, msg) in messages.iter().enumerate() {
        if msg.role == Role::System {
            continue;
        }
        count += 1;
        if count >= protect_first_n {
            return i + 1;
        }
    }
    // All messages are in the head.
    messages.len()
}

/// Walk backwards from the end, accumulating tokens up to
/// `protect_tail_tokens`. Returns the index of the first message in the
/// protected tail.
///
/// Keeps at least a small recent tail intact and, if the budget would
/// otherwise protect the entire post-head transcript, still forces a
/// non-empty middle region so manual compaction can do useful work.
pub fn find_tail_cut_by_tokens(
    messages: &[Message],
    head_end: usize,
    config: &CompressorConfig,
) -> usize {
    let post_head = messages.len().saturating_sub(head_end);
    if post_head <= 1 {
        return head_end;
    }

    let min_tail = (post_head - 1).min(3);
    let fallback_cut = messages.len() - min_tail;
    let tail_budget_chars = (config.protect_tail_tokens as f64 * 4.0) as usize;
    let mut remaining = tail_budget_chars;
    let mut tail_start = messages.len();

    for i in (head_end..messages.len()).rev() {
        let msg_chars = estimate_message_chars(&messages[i]);
        if msg_chars > remaining {
            // This message doesn't fit entirely — it becomes the boundary.
            // We include it in the tail but note that truncation may be needed.
            tail_start = i;
            break;
        }
        remaining -= msg_chars;
        tail_start = i;
    }

    tail_start = tail_start.min(fallback_cut);
    if tail_start <= head_end {
        tail_start = fallback_cut.max(head_end + 1);
    }

    tail_start
}

/// Rough character count for a message (content + tool call arguments).
pub fn estimate_message_chars(msg: &Message) -> usize {
    let content_chars = match &msg.content {
        Content::Text(s) => s.len(),
        Content::Parts(parts) => parts
            .iter()
            .map(|p| match p {
                hermes_core::message::ContentPart::Text { text } => text.len(),
                hermes_core::message::ContentPart::ImageUrl { url } => url.len(),
            })
            .sum(),
    };
    let reasoning_chars = msg.reasoning.as_ref().map_or(0, |s| s.len());
    let tool_calls_chars: usize = msg
        .tool_calls
        .as_ref()
        .map_or(0, |calls| calls.iter().map(|c| c.arguments.to_string().len()).sum());

    content_chars + reasoning_chars + tool_calls_chars
}

/// Estimate token count from character count.
pub fn estimate_tokens_from_chars(chars: usize, chars_per_token: f64) -> u64 {
    (chars as f64 / chars_per_token) as u64
}

/// Estimate total tokens for a slice of messages.
pub fn estimate_tokens(messages: &[Message], chars_per_token: f64) -> u64 {
    let total_chars: usize = messages.iter().map(|m| estimate_message_chars(m)).sum();
    estimate_tokens_from_chars(total_chars, chars_per_token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::message::{Content, Message, Role};

    fn user_msg(text: &str) -> Message {
        Message {
            role: Role::User,
            content: Content::Text(text.into()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    fn system_msg(text: &str) -> Message {
        Message {
            role: Role::System,
            content: Content::Text(text.into()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    fn tool_msg(tool_call_id: &str, text: &str) -> Message {
        Message {
            role: Role::Tool,
            content: Content::Text(text.into()),
            reasoning: None,
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: None,
        }
    }

    fn assistant_msg(text: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: Content::Text(text.into()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    fn test_config() -> CompressorConfig {
        CompressorConfig {
            threshold_percent: 0.50,
            protect_first_n: 3,
            protect_tail_tokens: 12_000,
            summary_target_ratio: 0.20,
        }
    }

    #[test]
    fn find_head_boundary_skips_system_messages() {
        let msgs = vec![
            system_msg("sys"),
            user_msg("a"),
            user_msg("b"),
            user_msg("c"),
            user_msg("d"),
        ];
        // protect_first_n=3 → should return index 4 (after a, b, c)
        assert_eq!(find_head_boundary(&msgs, 3), 4);
    }

    #[test]
    fn find_head_boundary_with_no_system() {
        let msgs = vec![user_msg("a"), user_msg("b"), user_msg("c")];
        assert_eq!(find_head_boundary(&msgs, 3), 3);
    }

    #[test]
    fn find_head_boundary_all_messages_when_fewer_than_protected() {
        let msgs = vec![system_msg("sys"), user_msg("a")];
        assert_eq!(find_head_boundary(&msgs, 3), 2);
    }

    #[test]
    fn find_tail_cut_forces_middle_when_budget_covers_everything() {
        let msgs = vec![
            system_msg("sys"),
            user_msg("u1"),
            assistant_msg("a1"),
            user_msg("u2"),
            assistant_msg("a2"),
            user_msg("u3"),
            assistant_msg("a3"),
        ];
        let config = CompressorConfig {
            protect_tail_tokens: 12_000,
            ..test_config()
        };

        let tail_start = find_tail_cut_by_tokens(&msgs, 2, &config);

        assert_eq!(tail_start, 4);
    }

    #[test]
    fn prune_old_tool_results_replaces_middle_tool_outputs() {
        let msgs = vec![
            system_msg("sys"),
            user_msg("hello"),
            tool_msg("call_1", "some very long tool output that should be pruned"),
            user_msg("followup"),
        ];
        let config = test_config();
        // With a large context, all messages should be in the tail — no pruning.
        let (pruned, _) = prune_old_tool_results(&msgs, &config);
        assert_eq!(pruned.len(), 4);

        // With a tiny tail, only the last message is protected.
        // Add extra messages after the tool msg so it falls outside the tail.
        let msgs2 = vec![
            system_msg("sys"),
            user_msg("hello"),
            tool_msg("call_1", "some very long tool output that should be pruned"),
            user_msg("followup"),
            assistant_msg("response"),
            user_msg("another"),
            assistant_msg("reply"),
        ];
        let tiny_config = CompressorConfig {
            protect_tail_tokens: 5, // protect only ~20 chars of tail
            ..test_config()
        };
        let (pruned, _chars_saved) = prune_old_tool_results(&msgs2, &tiny_config);
        // The tool message at index 2 should have been replaced
        if let Content::Text(ref t) = pruned[2].content {
            assert!(
                t.contains("[Old tool output cleared"),
                "expected pruned text, got: {t}"
            );
        } else {
            panic!("expected Text content");
        }
    }

    #[test]
    fn estimate_tokens_basic() {
        let msgs = vec![user_msg("hello world")]; // 11 chars
        let tokens = estimate_tokens(&msgs, 4.0);
        assert_eq!(tokens, 2); // 11 / 4 = 2.75 → 2
    }
}
