//! The agent loop — calls the LLM, reacts to `finish_reason`, dispatches
//! tools, returns a `RunResult`.
//!
//! Sub-modules:
//! - `metrics` — token-counting helpers + `validate_args`
//! - `compress` — single-lock compression orchestrator
//! - `run` — the state machine (`run`, `drive_turn`, `handle_finish_reason`, `dispatch_tool_calls`)

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;

use hermes_core::context_engine::{
    CompressionSkipReason, CompressionTrigger, ContextEngine,
};
use hermes_core::error::{LoopError, ProviderError, ToolError};
use hermes_core::message::{Message, ToolCall};
use hermes_core::provider::{Provider, ToolCallDelta};
use hermes_core::registry::InMemoryRegistry;
use hermes_core::tool::{ToolContext, ToolOutput};

mod compress;
mod metrics;
mod run;

// Re-export the public surface so `pub use loop_engine::*` keeps the
// same import path as before the split.
pub use metrics::estimate_tokens_for_messages;

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
    /// Optional context compression engine. None = no compression.
    pub context_engine: Option<Arc<TokioMutex<dyn ContextEngine>>>,
    /// Focus topic for manual `/compact [focus]`.
    pub focus_topic: Option<String>,
}

impl std::fmt::Debug for LoopConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopConfig")
            .field("max_iterations", &self.max_iterations)
            .field("max_duration", &self.max_duration)
            .field("system_prompt", &self.system_prompt)
            .field("context_engine", &"<dyn ContextEngine>")
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
            context_engine: None,
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
        tokens_before: u64,
        tokens_after: u64,
        summary_chars: usize,
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
    pub fn new(
        provider: impl Provider + 'static,
        registry: Arc<InMemoryRegistry>,
        config: LoopConfig,
    ) -> Self {
        Self::from_provider(Arc::new(provider), registry, config)
    }

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

    pub fn has_context_engine(&self) -> bool {
        self.config.context_engine.is_some()
    }

    pub async fn compact_messages(
        &self,
        mut messages: Vec<Message>,
        focus_topic: Option<&str>,
    ) -> Result<(Vec<Message>, LoopEvent), AgentRunError> {
        let Some(engine) = &self.config.context_engine else {
            return Ok((
                messages,
                LoopEvent::CompressionSkipped {
                    reason: CompressionSkipReason::Disabled,
                },
            ));
        };
        let outcome =
            compress::try_compress(engine, &mut messages, CompressionTrigger::Manual, focus_topic, self.config.focus_topic.as_deref(), true)
                .await
                .unwrap_or_else(|| compress::CompactOutcome::Failed {
                    error: "compression failed".into(),
                });
        let event = match outcome {
            compress::CompactOutcome::Compressed {
                tokens_before,
                tokens_after,
                summary_chars,
                duration,
            } => LoopEvent::CompressionCompleted {
                trigger: CompressionTrigger::Manual,
                tokens_before,
                tokens_after,
                summary_chars,
                duration,
            },
            compress::CompactOutcome::Skipped(reason) => LoopEvent::CompressionSkipped { reason },
            compress::CompactOutcome::Failed { error } => LoopEvent::CompressionFailed {
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
        cancel: CancellationToken,
        on_event: impl FnMut(LoopEvent) + Send,
    ) -> Result<RunResult, AgentRunError> {
        run::run(self, initial_messages, ctx, cancel, on_event).await
    }
}
