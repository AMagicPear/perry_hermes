//! Token usage reporting.

use serde::{Deserialize, Serialize};

/// Token counts for a single completion. Reported by providers in
/// `Completion.usage` and accumulated by the loop into `LoopMetrics`.
///
/// Field names match OpenAI's wire format (`prompt_tokens`,
/// `completion_tokens`, `prompt_tokens_details.cached_tokens`) so the
/// OpenAI provider can deserialize directly without a translation step.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(rename = "prompt_tokens")]
    pub input_tokens: u64,
    #[serde(rename = "completion_tokens")]
    pub output_tokens: u64,
    /// Cached input tokens (Anthropic prompt caching, OpenAI cached). These
    /// are part of prompt/context occupancy even when billed separately.
    #[serde(default, rename = "cached_tokens")]
    pub cached_input_tokens: u64,
}

impl Usage {
    /// `input_tokens + cached_input_tokens`. Matches the value used for
    /// `LoopEvent::ContextUsageUpdated` after a real provider response
    /// (cached tokens still occupy context).
    pub fn prompt_context_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.cached_input_tokens)
    }

    /// Sum of all token fields. Useful for cost dashboards and per-turn
    /// totals; not used for context-occupancy accounting.
    pub fn total(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cached_input_tokens)
            .saturating_add(self.output_tokens)
    }
}
