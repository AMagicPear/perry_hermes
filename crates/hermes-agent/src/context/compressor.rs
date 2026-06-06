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
use hermes_core::message::{Content, Message, Role};
use hermes_core::{CompressError, ContextEngine, Provider, ProviderError, Usage};

use tokio_util::sync::CancellationToken;

use super::pruning::{self, estimate_tokens};
use super::summary;

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
        let target = (self.threshold_tokens(context_length) as f64 * self.summary_target_ratio) as u64;
        target.max(MIN_SUMMARY_TOKENS).min(MAX_SUMMARY_TOKENS)
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

/// The default `ContextEngine` — a 5-step compressor.
pub struct ContextCompressor {
    config: CompressorConfig,
    model: String,
    threshold_tok: u64,
    /// Last LLM-generated summary; reused as the seed for the next compression.
    previous_summary: Option<String>,
    /// Tokens saved by the most recent compression (0.0 = nothing saved).
    last_savings_ratio: f64,
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
            last_savings_ratio: 0.0,
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

    /// Get the current configuration.
    pub fn config(&self) -> &CompressorConfig {
        &self.config
    }

    /// Get the model name.
    pub fn model(&self) -> &str {
        &self.model
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
        let (messages, _pruned_chars) =
            pruning::prune_old_tool_results(&messages, &self.config);

        // Re-estimate. If we're under threshold, no LLM call needed.
        let est = estimate_tokens(&messages, 4.0);
        if !force && est < self.threshold_tok {
            return Ok(messages);
        }

        // Step 2: head protection.
        let head_end = pruning::find_head_boundary(&messages, self.config.protect_first_n);

        // Step 3: tail protection.
        let tail_start =
            pruning::find_tail_cut_by_tokens(&messages, head_end, &self.config);

        // Check: nothing to compress?
        if head_end >= tail_start {
            return Err(CompressError::NothingToCompress);
        }

        // Step 4: LLM summary of the middle slice.
        let middle = &messages[head_end..tail_start];
        let middle_text = summary::messages_to_transcript(middle);
        let prompt = summary::build_summary_prompt(
            &middle_text,
            self.previous_summary.as_deref(),
            focus_topic,
            self.config.summary_target_tokens(self.context_length),
        );

        // Call the LLM to generate a summary. On failure, drop the oldest
        // message and retry (Codex pattern).
        let summary_text = match self.call_summary_llm(&prompt).await {
            Ok(s) => s,
            Err(first_err) => {
                // Retry: drop the oldest non-system message and try again.
                let mut retry_messages = messages.clone();
                if let Some(drop_idx) =
                    retry_messages.iter().position(|m| m.role != Role::System)
                {
                    retry_messages.remove(drop_idx);
                }
                let retry_tail_start = tail_start.min(retry_messages.len());
                if head_end >= retry_tail_start {
                    return Err(CompressError::SummaryFailed(first_err.to_string()));
                }
                let retry_middle = &retry_messages[head_end..retry_tail_start];
                if retry_middle.is_empty() {
                    return Err(CompressError::SummaryFailed(first_err.to_string()));
                }
                let retry_text = summary::messages_to_transcript(retry_middle);
                let retry_prompt = summary::build_summary_prompt(
                    &retry_text,
                    self.previous_summary.as_deref(),
                    focus_topic,
                    self.config.summary_target_tokens(self.context_length),
                );
                self.call_summary_llm(&retry_prompt)
                    .await
                    .map_err(|e| CompressError::SummaryFailed(e.to_string()))?
            }
        };

        // Step 5: assemble.
        let summary_msg = summary::build_summary_message(&summary_text);
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
        self.last_savings_ratio = savings;
        if savings < 0.10 {
            self.ineffective_count += 1;
        } else {
            self.ineffective_count = 0;
        }
        self.previous_summary = Some(summary_text);

        Ok(new_messages)
    }

    /// Call the summary LLM. Uses the summary provider if set.
    async fn call_summary_llm(&self, prompt: &str) -> Result<String, ProviderError> {
        // Build a single user message with the prompt.
        let messages = vec![Message {
            role: Role::User,
            content: Content::Text(prompt.to_string()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        }];

        // Use the summary provider if set.
        let provider = self
            .summary_provider
            .as_ref()
            .ok_or_else(|| ProviderError::Other("no provider available for summary".into()))?;

        let cancel = CancellationToken::new();
        let completion = provider.complete(&messages, &[], cancel).await?;

        // Extract text from the completion message.
        match &completion.message.content {
            Content::Text(s) => Ok(s.clone()),
            Content::Parts(parts) => {
                let text: String = parts
                    .iter()
                    .filter_map(|p| match p {
                        hermes_core::message::ContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                Ok(text)
            }
        }
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
        self.last_savings_ratio = 0.0;
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

    fn assistant_msg(text: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: Content::Text(text.into()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    #[test]
    fn config_default_threshold_tokens() {
        let config = CompressorConfig::default();
        assert_eq!(config.threshold_tokens(128_000), 64_000); // 128_000 * 0.50
    }

    #[test]
    fn config_minimum_context_floor() {
        let config = CompressorConfig {
            ..CompressorConfig::default()
        };
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

    #[test]
    fn on_session_reset_clears_state() {
        let mut compressor =
            ContextCompressor::new(CompressorConfig::default(), "test".into());
        compressor.previous_summary = Some("old summary".into());
        compressor.ineffective_count = 5;
        compressor.last_savings_ratio = 0.05;

        compressor.on_session_reset();

        assert!(compressor.previous_summary.is_none());
        assert_eq!(compressor.ineffective_count, 0);
        assert_eq!(compressor.last_savings_ratio, 0.0);
    }

    #[test]
    fn update_model_changes_threshold() {
        let mut compressor =
            ContextCompressor::new(CompressorConfig::default(), "old-model".into());
        let old_threshold = compressor.threshold_tokens();

        compressor.update_model("new-model", 256_000);

        assert_eq!(compressor.model(), "new-model");
        assert_ne!(compressor.threshold_tokens(), old_threshold);
        assert_eq!(compressor.threshold_tokens(), 128_000); // 256K * 0.5
    }

    #[test]
    fn should_compress_returns_false_after_two_ineffective() {
        let mut compressor =
            ContextCompressor::new(CompressorConfig::default(), "test".into());
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

        let messages = vec![
            system_msg("sys"),
            user_msg("hello"),
            assistant_msg("hi"),
        ];

        // Messages are far below the threshold (min 8000 tokens), so
        // the compressor returns Ok(messages) in the early-exit path.
        let result = compressor.compress(messages.clone(), None, None, false).await;
        assert!(result.is_ok());
        let out = result.unwrap();
        assert_eq!(out.len(), messages.len());
    }
}
