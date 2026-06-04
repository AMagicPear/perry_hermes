//! The agent loop — calls the LLM, reacts to `finish_reason`, returns a
//! `RunResult`.
//!
//! Phase 1 minimum: one iteration, handle `Stop` / `Length` /
//! `ContentFilter` / `Error` finish reasons, ignore `ToolUse` (tool
//! dispatch lands in phase 3+). See `plans/rust-port-design.md` §4 for
//! the full design; later phases will replace this minimum with the
//! full state machine.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use hermes_core::error::{LoopError, ProviderError, ToolError};
use hermes_core::message::{Message, ToolCall};
use hermes_core::provider::{FinishReason, Provider};
use hermes_core::registry::ToolRegistry;
use hermes_core::tool::ToolOutput;

/// The agent loop. Generic over `P: Provider` and `R: ToolRegistry` so
/// tests can swap in mocks. The loop holds a reference to the registry
/// (not ownership) so multiple loops can share it.
#[allow(dead_code)] // `registry` and `config` are wired in phase 3+ for
                    // tool dispatch, system-prompt injection, and budget
                    // enforcement; phase 1 minimum only exercises the
                    // single Stop path.
pub struct AgentLoop<P: Provider, R: ToolRegistry> {
    provider: P,
    registry: Arc<R>,
    config: LoopConfig,
}

/// Configuration for a single `run()` invocation.
#[derive(Debug, Clone)]
pub struct LoopConfig {
    /// Maximum number of LLM calls before the loop gives up.
    pub max_iterations: u32,
    /// Wall-clock cap.
    pub max_duration: Duration,
    /// Run tool calls in a batch in parallel? (phase 1: ignored)
    pub parallel_tool_calls: bool,
    /// Optional system prompt prepended to messages if not already present.
    pub system_prompt: Option<String>,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: 90,
            max_duration: Duration::from_secs(60 * 10),
            parallel_tool_calls: false,
            system_prompt: None,
        }
    }
}

/// Accumulated counts and timing for a single `run()` call.
#[derive(Debug, Clone, Default)]
pub struct LoopMetrics {
    pub iterations: u32,
    pub tool_calls: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub duration: Duration,
}

/// The return value of `run()`. Carries the final assistant message,
/// the full trajectory (so callers can compress / save / inspect), and
/// aggregate metrics.
#[derive(Debug, Clone)]
pub struct RunResult {
    pub final_message: Message,
    pub messages: Vec<Message>,
    pub metrics: LoopMetrics,
}

/// Side-channel events emitted as the loop progresses. The CLI uses
/// these to drive the spinner / activity feed; tests collect them via
/// the `on_event` callback.
#[derive(Debug, Clone)]
pub enum LoopEvent {
    Thinking,
    AssistantMessage(Message),
    ToolCallStarted {
        call: ToolCall,
        iteration: u32,
    },
    ToolCallFinished {
        call: ToolCall,
        result: Result<ToolOutput, ToolError>,
    },
    LengthLimit,
    IterationsExhausted,
    Cancelled,
}

impl<P: Provider, R: ToolRegistry> AgentLoop<P, R> {
    pub fn new(provider: P, registry: Arc<R>, config: LoopConfig) -> Self {
        Self {
            provider,
            registry,
            config,
        }
    }

    /// Run a full conversation. In phase 1 this is exactly one
    /// provider call; phase 3+ will turn it into the loop in §4 of the
    /// design doc.
    pub async fn run(
        &self,
        initial_messages: Vec<Message>,
        cancel: CancellationToken,
        mut on_event: impl FnMut(LoopEvent) + Send,
    ) -> Result<RunResult, LoopError> {
        let mut messages = initial_messages;
        let mut metrics = LoopMetrics::default();
        let started = Instant::now();

        if cancel.is_cancelled() {
            on_event(LoopEvent::Cancelled);
            return Err(LoopError::Cancelled);
        }

        on_event(LoopEvent::Thinking);
        let completion = self
            .provider
            .complete(&messages, &[], cancel.clone())
            .await?;
        metrics.iterations += 1;
        metrics.input_tokens += completion.usage.input_tokens;
        metrics.output_tokens += completion.usage.output_tokens;

        let assistant_msg = completion.message.clone();
        messages.push(assistant_msg.clone());
        on_event(LoopEvent::AssistantMessage(assistant_msg.clone()));

        match completion.finish_reason {
            FinishReason::Stop => {
                metrics.duration = started.elapsed();
                Ok(RunResult {
                    final_message: assistant_msg,
                    messages,
                    metrics,
                })
            }
            FinishReason::Length => {
                on_event(LoopEvent::LengthLimit);
                metrics.duration = started.elapsed();
                Ok(RunResult {
                    final_message: assistant_msg,
                    messages,
                    metrics,
                })
            }
            FinishReason::ContentFilter => Err(LoopError::ContentFilter),
            FinishReason::Error => Err(LoopError::Provider(ProviderError::Other(
                "provider returned finish_reason=error".into(),
            ))),
            FinishReason::ToolUse => Err(LoopError::Provider(ProviderError::Other(
                "tool calls not supported in phase 1 (agent loop minimum)"
                    .into(),
            ))),
        }
    }
}
