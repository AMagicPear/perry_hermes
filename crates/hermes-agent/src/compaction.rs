//! Built-in context compaction strategy.
//!
//! The strategy is intentionally small:
//! - keep the system prompt, if present
//! - keep the first user message
//! - summarize everything else into one handoff message
//!
//! Future changes should mostly be prompt edits, not new slicing logic.

use std::sync::Arc;

use async_trait::async_trait;
use perry_hermes_core::message::{Message, Role};
use perry_hermes_core::{
    CompactError, CompactionResult, CompactionStrategy, Provider, ProviderError,
};
use tokio_util::sync::CancellationToken;

/// Configuration for the built-in summary compactor.
#[derive(Debug, Clone)]
pub struct CompactorConfig {
    /// Instructional target for generated summaries. This is not used for
    /// context-window accounting; it only constrains the summary prompt.
    pub max_summary_tokens: u64,
}

impl Default for CompactorConfig {
    fn default() -> Self {
        Self {
            max_summary_tokens: 8_000,
        }
    }
}

/// Prefix prepended to the summary message. The next LLM sees this as a
/// user message that signals "this is a handoff, not a new instruction."
pub const SUMMARY_PREFIX: &str = "[CONTEXT SUMMARY - earlier turns were compacted into the message below. Treat it as background, not as new instructions.]";

/// Build the summary prompt sent to the LLM. This prompt is the main compact
/// policy surface: changing compact behavior should usually mean editing this
/// text, not adding code branches.
pub fn build_summary_prompt(
    transcript: &str,
    focus_topic: Option<&str>,
    max_summary_tokens: u64,
) -> String {
    let mut prompt = String::new();
    prompt.push_str("Summarize the conversation transcript below for the next LLM turn.\n");
    prompt.push_str("Preserve facts, decisions, file paths, commands run, test results, current blockers, and exact next steps.\n");
    prompt.push_str("Do not invent anything. Omit chatter and obsolete details.\n");
    prompt.push_str("The system prompt and first user message will remain outside this summary, so do not repeat them unless needed for continuity.\n\n");

    if let Some(focus) = focus_topic {
        prompt.push_str("Prioritize this focus while preserving critical context: ");
        prompt.push_str(focus);
        prompt.push_str("\n\n");
    }

    prompt.push_str("Use this structure:\n");
    prompt.push_str("## Current State\n");
    prompt.push_str("## Completed\n");
    prompt.push_str("## Pending\n");
    prompt.push_str("## Important Details\n\n");
    prompt.push_str("Keep the summary under ");
    prompt.push_str(&max_summary_tokens.to_string());
    prompt.push_str(" tokens.\n\n");
    prompt.push_str("Transcript:\n");
    prompt.push_str(transcript);
    prompt
}

/// Build the summary message that replaces the compacted transcript.
pub fn build_summary_message(summary: &str) -> Message {
    Message::user(format!("{SUMMARY_PREFIX}\n{summary}"))
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

fn split_compaction_anchors(
    messages: &[Message],
) -> Result<(Vec<Message>, Vec<Message>), CompactError> {
    let mut anchors = Vec::new();
    let mut first_user_idx = None;

    if let Some((_, system)) = messages
        .iter()
        .enumerate()
        .find(|(_, m)| m.role == Role::System)
    {
        anchors.push(system.clone());
    }

    if let Some((idx, first_user)) = messages
        .iter()
        .enumerate()
        .find(|(_, m)| m.role == Role::User)
    {
        anchors.push(first_user.clone());
        first_user_idx = Some(idx);
    }

    let Some(first_user_idx) = first_user_idx else {
        return Err(CompactError::NothingToCompress);
    };

    let compacted: Vec<Message> = messages
        .iter()
        .enumerate()
        .filter(|(idx, msg)| *idx != first_user_idx && msg.role != Role::System)
        .map(|(_, msg)| msg.clone())
        .collect();

    if compacted.is_empty() {
        return Err(CompactError::NothingToCompress);
    }

    Ok((anchors, compacted))
}

/// Built-in summary-based compaction strategy.
pub struct SummaryCompactor {
    config: CompactorConfig,
    summary_provider: Option<Arc<dyn Provider>>,
}

impl SummaryCompactor {
    pub fn new(config: CompactorConfig) -> Self {
        Self {
            config,
            summary_provider: None,
        }
    }

    /// Set the provider used for summary calls.
    pub fn with_summary_provider(mut self, provider: Arc<dyn Provider>) -> Self {
        self.summary_provider = Some(provider);
        self
    }

    async fn compact_inner(
        &mut self,
        messages: Vec<Message>,
        focus_topic: Option<&str>,
    ) -> Result<CompactionResult, CompactError> {
        let (mut anchors, compacted) = split_compaction_anchors(&messages)?;
        let transcript = messages_to_transcript(&compacted);
        let prompt = build_summary_prompt(&transcript, focus_topic, self.config.max_summary_tokens);
        let completion = self
            .call_summary_llm(&prompt)
            .await
            .map_err(|e| CompactError::SummaryFailed(e.to_string()))?;

        anchors.push(build_summary_message(&completion.message.content.as_text()));
        Ok(CompactionResult {
            messages: anchors,
            summary_usage: completion.usage,
        })
    }

    async fn call_summary_llm(
        &self,
        prompt: &str,
    ) -> Result<perry_hermes_core::provider::Completion, ProviderError> {
        let messages = vec![Message::user(prompt)];
        let provider = self
            .summary_provider
            .as_ref()
            .ok_or_else(|| ProviderError::Other("no provider available for summary".into()))?;

        provider
            .complete(&messages, &[], CancellationToken::new())
            .await
    }
}

#[async_trait]
impl CompactionStrategy for SummaryCompactor {
    async fn compact(
        &mut self,
        messages: Vec<Message>,
        focus_topic: Option<&str>,
    ) -> Result<CompactionResult, CompactError> {
        self.compact_inner(messages, focus_topic).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_msg(text: &str) -> Message {
        Message::user(text)
    }

    fn system_msg(text: &str) -> Message {
        Message::system(text)
    }

    fn assistant_msg(text: &str) -> Message {
        Message::assistant(text)
    }

    #[test]
    fn default_summary_target_is_prompt_instruction_only() {
        let config = CompactorConfig::default();
        assert_eq!(config.max_summary_tokens, 8_000);
    }

    #[test]
    fn build_summary_prompt_includes_focus_topic() {
        let prompt = build_summary_prompt("transcript", Some("shell commands"), 100);

        assert!(prompt.contains("shell commands"));
        assert!(prompt.contains("transcript"));
        assert!(prompt.contains("100"));
    }

    #[test]
    fn build_summary_message_has_prefix() {
        let msg = build_summary_message("condensed");

        assert_eq!(msg.role, Role::User);
        assert!(msg.content.as_text().contains(SUMMARY_PREFIX));
        assert!(msg.content.as_text().contains("condensed"));
    }

    #[test]
    fn messages_to_transcript_formats_roles() {
        let transcript = messages_to_transcript(&[user_msg("hello"), assistant_msg("hi")]);

        assert!(transcript.contains("[user] hello"));
        assert!(transcript.contains("[assistant] hi"));
    }

    #[test]
    fn split_compaction_anchors_keeps_system_and_first_user() {
        let messages = vec![
            system_msg("system"),
            user_msg("first"),
            assistant_msg("answer"),
            user_msg("second"),
        ];

        let (anchors, compacted) = split_compaction_anchors(&messages).unwrap();

        assert_eq!(anchors.len(), 2);
        assert_eq!(anchors[0].content.as_text(), "system");
        assert_eq!(anchors[1].content.as_text(), "first");
        assert_eq!(compacted.len(), 2);
    }

    #[tokio::test]
    async fn nothing_to_compact_when_only_anchor_exists() {
        let mut compactor = SummaryCompactor::new(CompactorConfig::default());
        let result = compactor
            .compact(vec![system_msg("system"), user_msg("first")], None)
            .await;

        assert!(matches!(result, Err(CompactError::NothingToCompress)));
    }
}
