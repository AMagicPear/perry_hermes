//! Context compaction orchestration for the agent loop.

use std::sync::Arc;
use std::time::{Duration, Instant};

use hermes_core::compaction_strategy::CompressionSkipReason;
use hermes_core::message::Message;
use hermes_core::{CompactError, CompactionStrategy};
use tokio::sync::Mutex as TokioMutex;

/// Outcome of one compaction attempt. The loop maps this to `LoopEvent`.
pub(crate) enum CompactOutcome {
    Skipped(CompressionSkipReason),
    Compressed {
        /// Total duration of the compaction call.
        duration: Duration,
        /// Provider-reported output tokens from the summary call.
        summary_output_tokens: u64,
    },
    Failed {
        error: String,
    },
}

/// Attempt one compaction pass. Holds the strategy lock for the full
/// `compact()` call and replaces `messages` in place on success.
pub(crate) async fn try_compact(
    strategy: &Arc<TokioMutex<dyn CompactionStrategy>>,
    messages: &mut Vec<Message>,
    focus_topic: Option<&str>,
    config_focus: Option<&str>,
) -> Option<CompactOutcome> {
    let started = Instant::now();

    let mut guard = match strategy.try_lock() {
        Ok(g) => g,
        Err(_) => {
            return Some(CompactOutcome::Skipped(
                CompressionSkipReason::NothingToCompress,
            ));
        }
    };
    let focus = config_focus.or(focus_topic);
    let result = guard.compact(messages.clone(), focus).await;
    drop(guard);

    let duration = started.elapsed();
    match result {
        Ok(result) => {
            let summary_output_tokens = result.summary_usage.output_tokens;
            *messages = result.messages;
            Some(CompactOutcome::Compressed {
                duration,
                summary_output_tokens,
            })
        }
        Err(CompactError::NothingToCompress) => Some(CompactOutcome::Skipped(
            CompressionSkipReason::NothingToCompress,
        )),
        Err(e) => Some(CompactOutcome::Failed {
            error: e.to_string(),
        }),
    }
}
