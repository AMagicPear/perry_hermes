//! The agent loop — calls the LLM, reacts to `finish_reason`, dispatches
//! tools, returns a `RunResult`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use hermes_core::error::{LoopError, ProviderError};
use hermes_core::message::{Content, Message, Role, ToolCall};
use hermes_core::provider::{FinishReason, Provider, ToolCallDelta};
use hermes_core::registry::InMemoryRegistry;
use hermes_core::tool::{ToolContext, ToolOutput};

pub struct AgentLoop {
    provider: Arc<dyn Provider>,
    registry: Arc<InMemoryRegistry>,
    config: LoopConfig,
}

#[derive(Debug, Clone)]
pub struct LoopConfig {
    pub max_iterations: u32,
    pub max_duration: Duration,
    pub system_prompt: Option<String>,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: 90,
            max_duration: Duration::from_secs(60 * 10),
            system_prompt: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LoopMetrics {
    pub iterations: u32,
    pub tool_calls: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub duration: Duration,
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
                messages.insert(
                    0,
                    Message {
                        role: Role::System,
                        content: Content::Text(sys.clone()),
                        reasoning: None,
                        tool_call_id: None,
                        tool_calls: None,
                    },
                );
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

            on_event(LoopEvent::Thinking);
            let mut stream = match self.provider.stream(&messages, &tools, cancel.clone()).await {
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
            metrics.output_tokens += completion.usage.output_tokens;

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
                            Ok(out) => Message {
                                role: Role::Tool,
                                content: Content::Text(out.content.clone()),
                                reasoning: None,
                                tool_call_id: Some(call.id.clone()),
                                tool_calls: None,
                            },
                            Err(e) => Message {
                                role: Role::Tool,
                                content: Content::Text(format!("Error: {e}")),
                                reasoning: None,
                                tool_call_id: Some(call.id.clone()),
                                tool_calls: None,
                            },
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
            messages.push(Message {
                role: Role::Assistant,
                content: Content::Text(error_text.clone()),
                reasoning: None,
                tool_call_id: None,
                tool_calls: None,
            });
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
