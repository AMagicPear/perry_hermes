//! Compression orchestration for the agent loop.
//!
//! `try_compress` holds the context engine's lock exactly once for the
//! entire `compress()` call (the old code dropped and re-acquired the
//! lock, which the original author flagged in a comment as a known smell).
//!
//! Returns an `CompactOutcome` describing the result; the caller maps
//! each variant to the right `LoopEvent`.

use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Mutex as TokioMutex;

use hermes_core::context_engine::{
    CompressError, CompressionSkipReason, CompressionTrigger, ContextEngine,
};
use hermes_core::message::Message;

use super::metrics::estimate_tokens_for_messages;

/// Outcome of a single compression attempt. Maps 1:1 to the
/// `CompressionCompleted` / `CompressionSkipped` / `CompressionFailed`
/// variants of `LoopEvent`.
pub(crate) enum CompactOutcome {
    Skipped(CompressionSkipReason),
    Compressed {
        tokens_before: u64,
        tokens_after: u64,
        summary_chars: usize,
        duration: std::time::Duration,
    },
    Failed { error: String },
}

/// Attempt one compression pass. Holds the engine's lock for the full
/// `compress()` call. On success, replaces `*messages` in place and
/// reports the size delta; the caller does not need to read the new
/// messages back.
///
/// `force` is forwarded to the engine — manual `/compact` calls pass
/// `true` so compression runs even when the threshold isn't reached.
pub(crate) async fn try_compress(
    engine: &Arc<TokioMutex<dyn ContextEngine>>,
    messages: &mut Vec<Message>,
    _trigger: CompressionTrigger,
    focus_topic: Option<&str>,
    config_focus: Option<&str>,
    force: bool,
) -> Option<CompactOutcome> {
    let started = Instant::now();
    let tokens_before = estimate_tokens_for_messages(messages, 4.0);

    let mut guard = match engine.try_lock() {
        Ok(g) => g,
        Err(_) => {
            return Some(CompactOutcome::Skipped(
                CompressionSkipReason::NothingToCompress,
            ));
        }
    };
    let focus = config_focus.or(focus_topic);
    let result = guard
        .compress(messages.clone(), Some(tokens_before), focus, force)
        .await;
    drop(guard);

    let duration = started.elapsed();
    match result {
        Ok(new_messages) => {
            let tokens_after = estimate_tokens_for_messages(&new_messages, 4.0);
            let summary_chars = new_messages
                .iter()
                .filter(|m| m.content.as_text().contains("[CONTEXT SUMMARY"))
                .map(|m| m.content.chars())
                .sum();
            *messages = new_messages;
            Some(CompactOutcome::Compressed {
                tokens_before,
                tokens_after,
                summary_chars,
                duration,
            })
        }
        Err(CompressError::NothingToCompress) => Some(CompactOutcome::Skipped(
            CompressionSkipReason::NothingToCompress,
        )),
        Err(e) => Some(CompactOutcome::Failed {
            error: e.to_string(),
        }),
    }
}
