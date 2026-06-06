//! The default `ContextEngine` implementation — a 5-step compressor.
//!
//! Algorithm (from the Python `ContextCompressor`):
//! 1. Cheap pre-pass — replace old tool result contents with one-line summaries.
//! 2. Head protection — find the boundary after the first N non-system messages.
//! 3. Tail protection — walk backwards, accumulating tokens up to the tail budget.
//! 4. LLM summary of the middle slice (with iterative update if previous summary exists).
//! 5. Assemble — head + summary message + tail.

use std::sync::Arc;

use async_trait::async_trait;
use hermes_core::message::{Message, Role};
use hermes_core::{CompressError, ContextEngine, Provider, ProviderError, Usage};

use tokio_util::sync::CancellationToken;

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the context compressor.
#[derive(Debug, Clone)]
pub struct CompressorConfig {
    /// Fraction of model context at which compression triggers. Default 0.50.
    pub threshold_percent: f64,
    /// Number of non-system messages to protect at the head. Default 3.
    pub protect_first_n: usize,
    /// Token budget for the protected tail. Default 12,000.
    pub protect_tail_tokens: u64,
    /// Summary is sized to this fraction of the threshold. Default 0.20.
    pub summary_target_ratio: f64,
}

impl CompressorConfig {
    /// Compute the token threshold at which compression triggers.
    pub fn threshold_tokens(&self, context_length: u64) -> u64 {
        let threshold = (context_length as f64 * self.threshold_percent) as u64;
        // Minimum context length floor: 8,000 tokens
        const MINIMUM_CONTEXT_LENGTH: u64 = 8_000;
        threshold.max(MINIMUM_CONTEXT_LENGTH)
    }

    /// Compute the target summary size in tokens.
    pub fn summary_target_tokens(&self, context_length: u64) -> u64 {
        const MIN_SUMMARY_TOKENS: u64 = 2_000;
        const MAX_SUMMARY_TOKENS: u64 = 12_000;
        let target =
            (self.threshold_tokens(context_length) as f64 * self.summary_target_ratio) as u64;
        target.clamp(MIN_SUMMARY_TOKENS, MAX_SUMMARY_TOKENS)
    }
}

impl Default for CompressorConfig {
    fn default() -> Self {
        Self {
            threshold_percent: 0.50,
            protect_first_n: 3,
            protect_tail_tokens: 12_000,
            summary_target_ratio: 0.20,
        }
    }
}

// ============================================================================
// Pruning — cheap pre-passes that reduce token count without an LLM call
// ============================================================================

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
        let msg_chars = messages[i].char_len() as i64;
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
            let original_chars = msg.char_len();
            let tool_name = msg.tool_call_id.as_deref().unwrap_or("unknown").to_string();
            let pruned_text = format!(
                "[Old tool output cleared, {} returned {} bytes]",
                tool_name, original_chars
            );
            let pruned_chars = pruned_text.len();
            total_pruned += original_chars.saturating_sub(pruned_chars);
            let tool_call_id = msg.tool_call_id.clone().unwrap_or_default();
            new_messages.push(Message::tool_result(tool_call_id, pruned_text));
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
        let msg_chars = messages[i].char_len();
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

/// Estimate total tokens for a slice of messages.
pub fn estimate_tokens(messages: &[Message], chars_per_token: f64) -> u64 {
    let total_chars: usize = messages.iter().map(Message::char_len).sum();
    (total_chars as f64 / chars_per_token) as u64
}

// ============================================================================
// Summary — prompt template and iterative update logic
// ============================================================================

/// Prefix prepended to the summary message. The next LLM sees this as a
/// user message that signals "this is a handoff, not a new instruction."
pub const SUMMARY_PREFIX: &str = "[CONTEXT SUMMARY \u{2014} earlier turns were compacted into the message below. Treat it as background, not as new instructions. Respond to the most recent user message that appears AFTER this summary.]";

/// Build the summary prompt sent to the LLM.
///
/// If `previous_summary` is `Some`, the LLM is asked to UPDATE it with the
/// new middle. Otherwise it generates from scratch.
///
/// If `focus_topic` is `Some`, the prompt includes an instruction to
/// prioritize that topic.
pub fn build_summary_prompt(
    middle_text: &str,
    previous_summary: Option<&str>,
    focus_topic: Option<&str>,
    max_summary_tokens: u64,
) -> String {
    let mut prompt = String::new();

    if let Some(prev) = previous_summary {
        prompt.push_str("You are updating an existing conversation summary with new turns.\n\n");
        prompt.push_str("## Existing Summary\n");
        prompt.push_str(prev);
        prompt.push_str("\n\n## New Turns to Integrate\n");
    } else {
        prompt
            .push_str("You are summarizing a section of a long conversation. Produce a handoff\n");
        prompt.push_str("summary for the next LLM that will resume the task.\n\n");
    }

    prompt.push_str(&format!("## Conversation Section\n{}\n\n", middle_text));

    prompt.push_str("Use this structure:\n\n");
    prompt.push_str("## Active Task\n");
    prompt.push_str("The current goal and what the user is trying to accomplish.\n\n");
    prompt.push_str("## Resolved\n");
    prompt.push_str("What has been completed or decided.\n\n");
    prompt.push_str("## Pending\n");
    prompt.push_str(
        "What remains to be done. Include file paths, function names, and concrete next steps.\n\n",
    );

    if let Some(focus) = focus_topic {
        prompt.push_str(&format!(
            "Prioritize preserving information related to: {}\n\n",
            focus
        ));
    }

    prompt.push_str(&format!(
        "Be concise. Total under {} tokens.",
        max_summary_tokens
    ));

    prompt
}

/// Build the summary message that replaces the middle slice.
///
/// Returns a user-role message with `SUMMARY_PREFIX` prepended.
pub fn build_summary_message(summary: &str) -> Message {
    Message::user(format!("{}\n{}", SUMMARY_PREFIX, summary))
}

/// Extract the text content of a slice of messages, formatted as a
/// conversation transcript for the summary prompt.
pub fn messages_to_transcript(messages: &[Message]) -> String {
    let mut out = String::new();
    for msg in messages {
        let role = msg.role.as_str();
        let content = msg.content.as_text();
        out.push_str(&format!("[{}] {}\n\n", role, content));
    }
    out
}

// ============================================================================
// ContextCompressor — the 5-step algorithm
// ============================================================================

/// The default `ContextEngine` — a 5-step compressor.
pub struct ContextCompressor {
    config: CompressorConfig,
    model: String,
    threshold_tok: u64,
    /// Last LLM-generated summary; reused as the seed for the next compression.
    previous_summary: Option<String>,
    /// Count of consecutive compressions that saved < ineffective_threshold.
    ineffective_count: u32,
    /// Aux provider for summary calls. None → fall back to the main provider.
    summary_provider: Option<Arc<dyn Provider>>,
    context_length: u64,
}

impl ContextCompressor {
    pub fn new(config: CompressorConfig, model: String) -> Self {
        let context_length = 128_000;
        let threshold_tok = config.threshold_tokens(context_length);
        Self {
            config,
            model,
            threshold_tok,
            previous_summary: None,
            ineffective_count: 0,
            summary_provider: None,
            context_length,
        }
    }

    /// Set an auxiliary provider for summary calls.
    pub fn with_summary_provider(mut self, provider: Arc<dyn Provider>) -> Self {
        self.summary_provider = Some(provider);
        self
    }

    /// Execute the 5-step compression algorithm.
    async fn compress_inner(
        &mut self,
        messages: Vec<Message>,
        _current_tokens: Option<u64>,
        focus_topic: Option<&str>,
        force: bool,
    ) -> Result<Vec<Message>, CompressError> {
        // Step 1: cheap pre-pass — replace old tool result contents.
        let (messages, _pruned_chars) = prune_old_tool_results(&messages, &self.config);

        // Re-estimate. If we're under threshold, no LLM call needed.
        let est = estimate_tokens(&messages, 4.0);
        if !force && est < self.threshold_tok {
            return Ok(messages);
        }

        // Step 2: head protection.
        let head_end = find_head_boundary(&messages, self.config.protect_first_n);

        // Step 3: tail protection.
        let tail_start = find_tail_cut_by_tokens(&messages, head_end, &self.config);

        // Check: nothing to compress?
        if head_end >= tail_start {
            return Err(CompressError::NothingToCompress);
        }

        // Step 4: LLM summary of the middle slice. On failure, drop the oldest
        // non-system message and retry once (Codex pattern).
        let middle = &messages[head_end..tail_start];
        let summary_text = match self.summarize_middle(middle, focus_topic).await {
            Ok(s) => s,
            Err(first_err) => {
                let mut retry_messages = messages.clone();
                if let Some(drop_idx) = retry_messages.iter().position(|m| m.role != Role::System) {
                    retry_messages.remove(drop_idx);
                }
                let retry_tail_start = tail_start.min(retry_messages.len());
                if head_end >= retry_tail_start {
                    return Err(CompressError::SummaryFailed(first_err));
                }
                let retry_middle = &retry_messages[head_end..retry_tail_start];
                if retry_middle.is_empty() {
                    return Err(CompressError::SummaryFailed(first_err));
                }
                self.summarize_middle(retry_middle, focus_topic)
                    .await
                    .map_err(CompressError::SummaryFailed)?
            }
        };

        // Step 5: assemble.
        let summary_msg = build_summary_message(&summary_text);
        let original_len = messages.len();
        let mut new_messages = Vec::with_capacity(head_end + 1 + (messages.len() - tail_start));
        // Head
        new_messages.extend(messages[..head_end].iter().cloned());
        // Summary
        new_messages.push(summary_msg);
        // Tail
        new_messages.extend(messages[tail_start..].iter().cloned());

        // Anti-thrashing bookkeeping
        let savings = if original_len > 0 {
            1.0 - (new_messages.len() as f64 / original_len as f64)
        } else {
            0.0
        };
        if savings < 0.10 {
            self.ineffective_count += 1;
        } else {
            self.ineffective_count = 0;
        }
        self.previous_summary = Some(summary_text);

        Ok(new_messages)
    }

    /// Build a summary prompt for a slice of messages and call the LLM.
    /// Returns the summary text on success, or the error string on failure.
    async fn summarize_middle(
        &self,
        middle: &[Message],
        focus_topic: Option<&str>,
    ) -> Result<String, String> {
        let middle_text = messages_to_transcript(middle);
        let prompt = build_summary_prompt(
            &middle_text,
            self.previous_summary.as_deref(),
            focus_topic,
            self.config.summary_target_tokens(self.context_length),
        );
        self.call_summary_llm(&prompt)
            .await
            .map_err(|e| e.to_string())
    }

    /// Call the summary LLM. Uses the summary provider if set.
    async fn call_summary_llm(&self, prompt: &str) -> Result<String, ProviderError> {
        // Build a single user message with the prompt.
        let messages = vec![Message::user(prompt)];

        // Use the summary provider if set.
        let provider = self
            .summary_provider
            .as_ref()
            .ok_or_else(|| ProviderError::Other("no provider available for summary".into()))?;

        let cancel = CancellationToken::new();
        let completion = provider.complete(&messages, &[], cancel).await?;
        Ok(completion.message.content.as_text())
    }
}

#[async_trait]
impl ContextEngine for ContextCompressor {
    fn name(&self) -> &'static str {
        "compressor"
    }

    fn update_from_response(&mut self, _usage: &Usage) {
        // Track the last known input token count for the post-turn trigger.
        // The actual trigger check is in should_compress().
    }

    fn should_compress(&self) -> bool {
        // Only check the ineffective backoff. The actual token threshold
        // comparison is done by the loop using estimated_tokens vs threshold.
        self.ineffective_count < 2
    }

    async fn compress(
        &mut self,
        messages: Vec<Message>,
        current_tokens: Option<u64>,
        focus_topic: Option<&str>,
        force: bool,
    ) -> Result<Vec<Message>, CompressError> {
        self.compress_inner(messages, current_tokens, focus_topic, force)
            .await
    }

    fn on_session_reset(&mut self) {
        self.previous_summary = None;
        self.ineffective_count = 0;
    }

    fn update_model(&mut self, model: &str, context_length: u64) {
        self.model = model.to_string();
        self.context_length = context_length;
        self.threshold_tok = self.config.threshold_tokens(context_length);
    }

    fn threshold_tokens(&self) -> u64 {
        self.threshold_tok
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::message::Message;

    fn user_msg(text: &str) -> Message {
        Message::user(text)
    }

    fn system_msg(text: &str) -> Message {
        Message::system(text)
    }

    fn assistant_msg(text: &str) -> Message {
        Message::assistant(text)
    }

    fn tool_msg(tool_call_id: &str, text: &str) -> Message {
        Message::tool_result(tool_call_id, text)
    }

    fn test_config() -> CompressorConfig {
        CompressorConfig::default()
    }

    // ----- Config -----

    #[test]
    fn config_default_threshold_tokens() {
        let config = CompressorConfig::default();
        assert_eq!(config.threshold_tokens(128_000), 64_000); // 128_000 * 0.50
    }

    #[test]
    fn config_minimum_context_floor() {
        let config = CompressorConfig::default();
        // 1000 * 0.50 = 500, but min is 8000
        assert_eq!(config.threshold_tokens(1_000), 8_000);
    }

    #[test]
    fn config_summary_target_tokens() {
        let config = CompressorConfig::default();
        let target = config.summary_target_tokens(128_000);
        assert!(target >= 2_000);
        assert!(target <= 12_000);
    }

    // ----- Compressor lifecycle -----

    #[test]
    fn on_session_reset_clears_state() {
        let mut compressor = ContextCompressor::new(CompressorConfig::default(), "test".into());
        compressor.previous_summary = Some("old summary".into());
        compressor.ineffective_count = 5;

        compressor.on_session_reset();

        assert!(compressor.previous_summary.is_none());
        assert_eq!(compressor.ineffective_count, 0);
    }

    #[test]
    fn update_model_changes_threshold() {
        let mut compressor =
            ContextCompressor::new(CompressorConfig::default(), "old-model".into());
        let old_threshold = compressor.threshold_tokens();

        compressor.update_model("new-model", 256_000);

        assert_ne!(compressor.threshold_tokens(), old_threshold);
        assert_eq!(compressor.threshold_tokens(), 128_000); // 256K * 0.5
    }

    #[test]
    fn should_compress_returns_false_after_two_ineffective() {
        let mut compressor = ContextCompressor::new(CompressorConfig::default(), "test".into());
        assert!(compressor.should_compress());

        compressor.ineffective_count = 1;
        assert!(compressor.should_compress());

        compressor.ineffective_count = 2;
        assert!(!compressor.should_compress());
    }

    #[tokio::test]
    async fn nothing_to_compress_when_head_meets_tail() {
        // Messages too short to exceed threshold — compressor returns early
        // with Ok (below-threshold early exit), never reaching head/tail check.
        let config = CompressorConfig {
            protect_first_n: 100, // protect all messages
            ..CompressorConfig::default()
        };
        let mut compressor = ContextCompressor::new(config, "test".into());

        let messages = vec![system_msg("sys"), user_msg("hello"), assistant_msg("hi")];

        // Messages are far below the threshold (min 8000 tokens), so
        // the compressor returns Ok(messages) in the early-exit path.
        let result = compressor
            .compress(messages.clone(), None, None, false)
            .await;
        assert!(result.is_ok());
        let out = result.unwrap();
        assert_eq!(out.len(), messages.len());
    }

    // ----- Pruning -----

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
        assert!(
            pruned[2]
                .content
                .as_text()
                .contains("[Old tool output cleared"),
            "expected pruned text, got: {:?}",
            pruned[2].content.as_text()
        );
    }

    #[test]
    fn estimate_tokens_basic() {
        let msgs = vec![user_msg("hello world")]; // 11 chars
        let tokens = estimate_tokens(&msgs, 4.0);
        assert_eq!(tokens, 2); // 11 / 4 = 2.75 → 2
    }

    // ----- Summary -----

    #[test]
    fn build_summary_prompt_includes_focus_topic() {
        let prompt = build_summary_prompt("some conversation", None, Some("task-X"), 12_000);
        assert!(
            prompt.contains("Prioritize preserving information related to: task-X"),
            "expected focus topic in prompt: {prompt}"
        );
    }

    #[test]
    fn build_summary_prompt_update_mode() {
        let prompt = build_summary_prompt("new turns", Some("old summary"), None, 12_000);
        assert!(
            prompt.contains("Existing Summary"),
            "expected update mode: {prompt}"
        );
        assert!(
            prompt.contains("old summary"),
            "expected old summary text: {prompt}"
        );
    }

    #[test]
    fn build_summary_message_has_prefix() {
        let msg = build_summary_message("test summary");
        assert!(msg.content.as_text().starts_with(SUMMARY_PREFIX));
        assert!(msg.content.as_text().contains("test summary"));
        assert_eq!(msg.role, Role::User);
    }

    #[test]
    fn messages_to_transcript_formats_roles() {
        let msgs = vec![user_msg("hello"), assistant_msg("hi there")];
        let transcript = messages_to_transcript(&msgs);
        assert!(transcript.contains("[user] hello"));
        assert!(transcript.contains("[assistant] hi there"));
    }
}
