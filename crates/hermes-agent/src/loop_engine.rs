//! The agent loop — calls the LLM, reacts to `finish_reason`, dispatches
//! tools, returns a `RunResult`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;

use hermes_core::context_engine::{
    CompressError, CompressionSkipReason, CompressionTrigger, ContextEngine,
};
use hermes_core::error::{LoopError, ProviderError};
use hermes_core::message::{Message, Role, ToolCall};
use hermes_core::provider::{FinishReason, Provider, ToolCallDelta};
use hermes_core::registry::InMemoryRegistry;
use hermes_core::tool::{ToolContext, ToolOutput};
use hermes_core::Usage;

pub struct AgentLoop {
    provider: Arc<dyn Provider>,
    registry: Arc<InMemoryRegistry>,
    config: LoopConfig,
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
        result: Result<ToolOutput, hermes_core::error::ToolError>,
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
        let mut metrics = LoopMetrics::default();
        let event = self
            .try_compress(
                engine,
                CompressionTrigger::Manual,
                &mut messages,
                focus_topic,
                &mut metrics,
                true,
            )
            .await
            .unwrap_or(LoopEvent::CompressionFailed {
                trigger: CompressionTrigger::Manual,
                error: "compression failed".into(),
            });
        Ok((messages, event))
    }

    pub async fn run(
        &self,
        initial_messages: Vec<Message>,
        ctx: ToolContext,
        cancel: CancellationToken,
        mut on_event: impl FnMut(LoopEvent) + Send,
    ) -> Result<RunResult, AgentRunError> {
        let initial_len = initial_messages.len();
        let mut messages = initial_messages;
        let mut metrics = LoopMetrics::default();
        let started = Instant::now();

        if let Some(sys) = &self.config.system_prompt {
            if !messages.iter().any(|m| m.role == Role::System) {
                messages.insert(0, Message::system(sys.clone()));
            }
        }

        // Pre-turn compression check.
        if let Some(engine) = &self.config.context_engine {
            let estimated = estimate_tokens_for_messages(&messages, 4.0);
            let should = {
                let guard = engine.lock().await;
                guard.should_compress() && estimated >= guard.threshold_tokens()
            };
            if should {
                if let Some(event) = self
                    .try_compress(
                        engine,
                        CompressionTrigger::PreTurn,
                        &mut messages,
                        None,
                        &mut metrics,
                        false,
                    )
                    .await
                {
                    on_event(event);
                }
            }
        }

        loop {
            if cancel.is_cancelled() {
                on_event(LoopEvent::Cancelled);
                return Err(AgentRunError::Loop(LoopError::Cancelled));
            }
            if metrics.iterations >= self.config.max_iterations {
                on_event(LoopEvent::IterationsExhausted);
                return Err(AgentRunError::Loop(LoopError::MaxIterations(
                    metrics.iterations,
                )));
            }
            if started.elapsed() > self.config.max_duration {
                return Err(AgentRunError::Loop(LoopError::Timeout(started.elapsed())));
            }

            let tools = self.registry.schemas();
            let estimated_context_tokens = estimate_request_context_tokens(&messages, &tools, 4.0);
            on_event(LoopEvent::ContextUsageUpdated {
                used_tokens: estimated_context_tokens,
            });

            on_event(LoopEvent::Thinking);
            let mut stream = match self
                .provider
                .stream(&messages, &tools, cancel.clone())
                .await
            {
                Ok(stream) => stream,
                Err(e) => {
                    return Err(Self::provider_failure(messages, None, initial_len, e));
                }
            };
            let mut acc = hermes_core::provider::StreamAccumulator::new();
            let completion = loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        on_event(LoopEvent::Cancelled);
                        return if acc.is_empty() {
                            Err(AgentRunError::Loop(LoopError::Cancelled))
                        } else {
                            Err(AgentRunError::Loop(LoopError::CancelledWith(
                                acc.into_partial_message(Role::Assistant),
                            )))
                        };
                    }
                    chunk = stream.next() => {
                        match chunk {
                            Some(Ok(delta)) => {
                                if let Some(s) = &delta.content_delta {
                                    on_event(LoopEvent::ContentDelta(s.clone()));
                                }
                                if let Some(s) = &delta.reasoning_delta {
                                    on_event(LoopEvent::ReasoningDelta(s.clone()));
                                }
                                if let Some(td) = &delta.tool_call_delta {
                                    on_event(LoopEvent::ToolCallPartial(td.clone()));
                                }
                                acc.add(&delta);
                            }
                            Some(Err(e)) => {
                                return Err(Self::provider_failure(
                                    messages.clone(),
                                    if acc.is_empty() {
                                        None
                                    } else {
                                        Some(acc.into_partial_message(Role::Assistant))
                                    },
                                    initial_len,
                                    e,
                                ));
                            }
                            None => break acc.finalize(),
                        }
                    }
                }
            };
            metrics.iterations += 1;
            metrics.input_tokens += completion.usage.input_tokens;
            metrics.cached_input_tokens += completion.usage.cached_input_tokens;
            metrics.output_tokens += completion.usage.output_tokens;
            let prompt_context_tokens = prompt_context_tokens_from_usage(completion.usage);
            if prompt_context_tokens > 0 {
                on_event(LoopEvent::ContextUsageUpdated {
                    used_tokens: prompt_context_tokens,
                });
            }

            let assistant_msg = completion.message.clone();
            messages.push(assistant_msg.clone());
            on_event(LoopEvent::AssistantMessage(assistant_msg.clone()));

            match completion.finish_reason {
                FinishReason::Stop => {
                    metrics.duration = started.elapsed();
                    return Ok(RunResult {
                        final_message: assistant_msg,
                        messages,
                        metrics,
                    });
                }
                FinishReason::Length => {
                    on_event(LoopEvent::LengthLimit);
                    metrics.duration = started.elapsed();
                    return Ok(RunResult {
                        final_message: assistant_msg,
                        messages,
                        metrics,
                    });
                }
                FinishReason::ContentFilter => {
                    return Err(AgentRunError::Loop(LoopError::ContentFilter))
                }
                FinishReason::Error => {
                    return Err(AgentRunError::Loop(LoopError::Provider(
                        ProviderError::Other("provider returned finish_reason=error".into()),
                    )));
                }
                FinishReason::ToolUse => {
                    let calls = assistant_msg.tool_calls.clone().unwrap_or_default();
                    if calls.is_empty() {
                        return Err(AgentRunError::Loop(LoopError::Provider(
                            ProviderError::InvalidResponse(
                                "finish_reason=tool_use but no tool_calls".into(),
                            ),
                        )));
                    }

                    if let Some(engine) = &self.config.context_engine {
                        if prompt_context_tokens > 0 {
                            let should = {
                                let guard = engine.lock().await;
                                guard.should_compress()
                                    && prompt_context_tokens >= guard.threshold_tokens()
                            };
                            if should {
                                if let Some(event) = self
                                    .try_compress(
                                        engine,
                                        CompressionTrigger::PostTurn,
                                        &mut messages,
                                        None,
                                        &mut metrics,
                                        false,
                                    )
                                    .await
                                {
                                    on_event(event);
                                }
                            }
                        }
                    }

                    for call in calls {
                        if cancel.is_cancelled() {
                            on_event(LoopEvent::Cancelled);
                            return Err(AgentRunError::Loop(LoopError::Cancelled));
                        }
                        on_event(LoopEvent::ToolCallStarted {
                            call: call.clone(),
                            iteration: metrics.iterations,
                        });

                        let result = self.dispatch_tool(&call, &ctx, cancel.clone()).await;

                        let tool_msg = match &result {
                            Ok(out) => Message::tool_result(call.id.clone(), out.content.clone()),
                            Err(e) => Message::tool_result(call.id.clone(), format!("Error: {e}")),
                        };
                        messages.push(tool_msg);
                        on_event(LoopEvent::ToolCallFinished {
                            call: call.clone(),
                            result,
                        });
                        metrics.tool_calls += 1;
                    }
                }
            }
        }
    }

    /// Attempt compression. Returns `Some(event)` if compression ran
    /// (success or skip), `None` if the engine is already locked.
    async fn try_compress(
        &self,
        engine: &Arc<TokioMutex<dyn ContextEngine>>,
        trigger: CompressionTrigger,
        messages: &mut Vec<Message>,
        focus_topic: Option<&str>,
        metrics: &mut LoopMetrics,
        force: bool,
    ) -> Option<LoopEvent> {
        let guard = match engine.try_lock() {
            Ok(g) => g,
            Err(_) => {
                return Some(LoopEvent::CompressionSkipped {
                    reason: CompressionSkipReason::NothingToCompress,
                });
            }
        };

        let tokens_before = estimate_tokens_for_messages(messages, 4.0);
        let started = Instant::now();

        // Drop the guard before calling compress since it needs &mut self.
        // We need to restructure: get the data we need, then call.
        // Actually, we hold the guard for the entire operation.
        drop(guard);

        // Re-lock with await for the actual compression.
        let mut guard = engine.lock().await;
        let focus = self.config.focus_topic.as_deref().or(focus_topic);
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
                    .sum::<usize>();

                *messages = new_messages;
                metrics.compressions += 1;

                Some(LoopEvent::CompressionCompleted {
                    trigger,
                    tokens_before,
                    tokens_after,
                    summary_chars,
                    duration,
                })
            }
            Err(CompressError::NothingToCompress) => Some(LoopEvent::CompressionSkipped {
                reason: CompressionSkipReason::NothingToCompress,
            }),
            Err(e) => Some(LoopEvent::CompressionFailed {
                trigger,
                error: e.to_string(),
            }),
        }
    }

    async fn dispatch_tool(
        &self,
        call: &ToolCall,
        ctx: &ToolContext,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, hermes_core::error::ToolError> {
        let tool = self
            .registry
            .get(&call.name)
            .ok_or_else(|| hermes_core::error::ToolError::NotFound(call.name.clone()))?;

        let args = validate_args(&call.arguments, tool.parameters_schema())
            .map_err(|e| hermes_core::error::ToolError::InvalidArgs(e.to_string()))?;

        tool.execute(args, ctx.clone(), cancel).await
    }

    fn provider_failure(
        mut messages: Vec<Message>,
        partial_assistant: Option<Message>,
        initial_len: usize,
        error: ProviderError,
    ) -> AgentRunError {
        if let Some(msg) = partial_assistant {
            messages.push(msg);
        }
        if messages.len() > initial_len {
            let error_text = format!("Turn interrupted by error: provider error: {error}");
            messages.push(Message::assistant(error_text.clone()));
            AgentRunError::FailedTurn {
                failed_turn: FailedTurn {
                    messages,
                    error: error_text,
                },
                source: error,
            }
        } else {
            AgentRunError::Loop(LoopError::Provider(error))
        }
    }
}

/// Estimate total tokens for a list of messages.
pub fn estimate_tokens_for_messages(messages: &[Message], chars_per_token: f64) -> u64 {
    let total_chars: usize = messages.iter().map(Message::char_len).sum();
    (total_chars as f64 / chars_per_token) as u64
}

fn prompt_context_tokens_from_usage(usage: Usage) -> u64 {
    usage.input_tokens.saturating_add(usage.cached_input_tokens)
}

fn estimate_request_context_tokens(
    messages: &[Message],
    tools: &[hermes_core::registry::ToolSchema],
    chars_per_token: f64,
) -> u64 {
    let message_chars: usize = messages.iter().map(Message::char_len).sum();
    let tool_chars: usize = tools
        .iter()
        .filter_map(|tool| serde_json::to_string(tool).ok())
        .map(|s| s.len())
        .sum();
    ((message_chars + tool_chars) as f64 / chars_per_token) as u64
}

fn validate_args(
    args: &serde_json::Value,
    schema: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use jsonschema::JSONSchema;
    let compiled = JSONSchema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .compile(&schema)
        .map_err(|e| format!("schema compile: {e}"))?;
    let result = compiled.validate(args);
    if let Err(errors) = result {
        let msgs: Vec<String> = errors.map(|e| e.to_string()).collect();
        return Err(msgs.join("; "));
    }
    Ok(args.clone())
}
