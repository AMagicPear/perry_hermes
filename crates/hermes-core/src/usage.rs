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
    /// Cached input tokens (Anthropic prompt caching, OpenAI cached). For
    /// cost reporting.
    #[serde(default, rename = "cached_tokens")]
    pub cached_input_tokens: u64,
}
