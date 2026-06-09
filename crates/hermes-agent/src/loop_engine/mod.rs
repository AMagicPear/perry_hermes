//! The agent loop — calls the LLM, reacts to `finish_reason`, dispatches
//! tools, returns a `RunResult`.
//!
//! Sub-modules:
//! - `metrics` — provider usage helpers + `validate_args`
//! - `run` — the state machine (`run`, `drive_turn`, `handle_finish_reason`, `dispatch_tool_calls`)

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;

use perry_hermes_core::compaction_strategy::{
    CompactionStrategy, CompressionSkipReason, CompressionTrigger,
};
use perry_hermes_core::error::{LoopError, ProviderError, ToolError};
use perry_hermes_core::message::{Message, ToolCall};
use perry_hermes_core::provider::{Provider, ToolCallDelta};
use perry_hermes_core::registry::InMemoryRegistry;
use perry_hermes_core::tool::{ToolContext, ToolOutput};

use crate::session::AgentSession;

mod metrics;
mod run;

pub struct AgentLoop {
    pub(crate) provider: Arc<dyn Provider>,
    pub(crate) registry: Arc<InMemoryRegistry>,
    pub(crate) config: LoopConfig,
}

#[derive(Clone)]
pub struct LoopConfig {
    pub max_iterations: u32,
    pub max_duration: Duration,
    pub system_prompt: Option<String>,
    /// Optional context compaction strategy. None = no compaction.
    pub compaction_strategy: Option<Arc<TokioMutex<dyn CompactionStrategy>>>,
    /// Model context window and compression threshold used with real
    /// provider usage.
    pub context_window: Option<ContextWindow>,
    /// Focus topic for manual `/compact [focus]`.
    pub focus_topic: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct ContextWindow {
    pub max_tokens: u64,
    pub compression_threshold_ratio: f64,
}

impl ContextWindow {
    pub fn threshold_tokens(self) -> u64 {
        (self.max_tokens as f64 * self.compression_threshold_ratio) as u64
    }

    pub fn should_compress(self, used_tokens: u64) -> bool {
        used_tokens >= self.threshold_tokens()
    }
}

impl std::fmt::Debug for LoopConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopConfig")
            .field("max_iterations", &self.max_iterations)
            .field("max_duration", &self.max_duration)
            .field("system_prompt", &self.system_prompt)
            .field("compaction_strategy", &"<dyn CompactionStrategy>")
            .field("context_window", &self.context_window)
            .field("focus_topic", &self.focus_topic)
            .finish()
    }
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: 90,
            max_duration: Duration::from_secs(60 * 10),
            system_prompt: None,
            compaction_strategy: None,
            context_window: None,
            focus_topic: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LoopMetrics {
    pub iterations: u32,
    pub tool_calls: u32,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub duration: Duration,
    pub compressions: u32,
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub final_message: Message,
    pub messages: Vec<Message>,
    pub metrics: LoopMetrics,
}

#[derive(Debug, Clone)]
pub struct FailedTurn {
    pub messages: Vec<Message>,
    pub error: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentRunError {
    #[error(transparent)]
    Loop(#[from] LoopError),
    #[error("provider error with partial response: {source}")]
    FailedTurn {
        failed_turn: FailedTurn,
        #[source]
        source: ProviderError,
    },
}

#[derive(Debug, Clone)]
pub enum LoopEvent {
    Thinking,
    ContentDelta(String),
    ReasoningDelta(String),
    ToolCallPartial(ToolCallDelta),
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
    ContextUsageUpdated {
        used_tokens: u64,
    },
    CompressionCompleted {
        trigger: CompressionTrigger,
        /// Provider-reported prompt context tokens that caused automatic
        /// compression. `None` for manual `/compact`.
        context_tokens: Option<u64>,
        /// Best known post-compaction context usage, derived from the first
        /// prompt-context baseline plus the summary call's output tokens.
        compacted_tokens: Option<u64>,
        duration: Duration,
    },
    CompressionSkipped {
        reason: CompressionSkipReason,
    },
    CompressionFailed {
        trigger: CompressionTrigger,
        error: String,
    },
}

impl AgentLoop {
    pub fn from_provider(
        provider: Arc<dyn Provider>,
        registry: Arc<InMemoryRegistry>,
        config: LoopConfig,
    ) -> Self {
        Self {
            provider,
            registry,
            config,
        }
    }

    pub async fn compact_messages(
        &self,
        mut messages: Vec<Message>,
        focus_topic: Option<&str>,
        session: &AgentSession,
    ) -> Result<(Vec<Message>, LoopEvent), AgentRunError> {
        let Some(engine) = &self.config.compaction_strategy else {
            return Ok((
                messages,
                LoopEvent::CompressionSkipped {
                    reason: CompressionSkipReason::Disabled,
                },
            ));
        };
        let outcome = crate::compaction::try_compact(
            engine,
            &mut messages,
            focus_topic,
            self.config.focus_topic.as_deref(),
        )
        .await
        .unwrap_or_else(|| crate::compaction::CompactOutcome::Failed {
            error: "compression failed".into(),
        });
        let event = match outcome {
            crate::compaction::CompactOutcome::Compressed {
                duration,
                summary_output_tokens,
            } => {
                let compacted_tokens = session
                    .compacted_context_tokens(summary_output_tokens)
                    .await;
                LoopEvent::CompressionCompleted {
                    trigger: CompressionTrigger::Manual,
                    context_tokens: None,
                    compacted_tokens,
                    duration,
                }
            }
            crate::compaction::CompactOutcome::Skipped(reason) => {
                LoopEvent::CompressionSkipped { reason }
            }
            crate::compaction::CompactOutcome::Failed { error } => LoopEvent::CompressionFailed {
                trigger: CompressionTrigger::Manual,
                error,
            },
        };
        Ok((messages, event))
    }

    pub async fn run(
        &self,
        initial_messages: Vec<Message>,
        ctx: ToolContext,
        session: &AgentSession,
        cancel: CancellationToken,
        on_event: impl FnMut(LoopEvent) + Send,
    ) -> Result<RunResult, AgentRunError> {
        run::run(self, initial_messages, ctx, session, cancel, on_event).await
    }
}
