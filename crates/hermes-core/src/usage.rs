//! Token usage reporting.

use serde::{Deserialize, Serialize};

/// Token counts for a single completion. Reported by providers in
/// `Completion.usage` and accumulated by the loop into `LoopMetrics`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Cached input tokens (Anthropic prompt caching, OpenAI cached). For
    /// cost reporting.
    pub cached_input_tokens: u64,
}