//! The agent loop's state machine.
//!
//! `run()` is the public entry point. It owns the iteration counter, the
//! message history, and the `on_event` callback. Sub-responsibilities are
//! split into private helpers:
//!
//! - `drive_turn` — start a provider stream, accumulate deltas, return
//!   the final `Completion` (or surface a provider error wrapped in a
//!   `FailedTurn`).
//! - `handle_finish_reason` — react to `Stop` / `Length` / `ToolUse` / etc.
//! - `dispatch_tool_calls` — run the tool calls in order, push tool
//!   results back into history.
//! - `pre_turn_compression_check` / `post_turn_compression_check` —
//!   `ContextEngine` triggers that fire at iteration boundaries.
//! - `build_failed_turn` — turn a partial `Completion` + provider error
//!   into a `FailedTurn` for history preservation.

use std::time::Instant;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use hermes_core::context_engine::CompressionTrigger;
use hermes_core::error::{LoopError, ProviderError, ToolError};
use hermes_core::message::{Message, Role, ToolCall};
use hermes_core::provider::{Completion, FinishReason, StreamAccumulator};
use hermes_core::tool::ToolContext;

use super::compressor::{try_compress, CompactOutcome};
use super::metrics::{prompt_context_tokens_from_usage, validate_args};
use super::{AgentLoop, AgentRunError, FailedTurn, LoopEvent, LoopMetrics, RunResult};

pub(crate) async fn run(
    engine: &AgentLoop,
    initial_messages: Vec<Message>,
    ctx: ToolContext,
    cancel: CancellationToken,
    mut on_event: impl FnMut(LoopEvent) + Send,
) -> Result<RunResult, AgentRunError> {
    let initial_len = initial_messages.len();
    let mut messages = initial_messages;
    let mut metrics = LoopMetrics::default();
    let started = Instant::now();

    if let Some(sys) = &engine.config.system_prompt {
        if !messages.iter().any(|m| m.role == Role::System) {
            messages.insert(0, Message::system(sys.clone()));
        }
    }

    loop {
        if cancel.is_cancelled() {
            on_event(LoopEvent::Cancelled);
            return Err(AgentRunError::Loop(LoopError::Cancelled));
        }
        if metrics.iterations >= engine.config.max_iterations {
            on_event(LoopEvent::IterationsExhausted);
            return Err(AgentRunError::Loop(LoopError::MaxIterations(
                metrics.iterations,
            )));
        }
        if started.elapsed() > engine.config.max_duration {
            return Err(AgentRunError::Loop(LoopError::Timeout(started.elapsed())));
        }

        let tools = engine.registry.schemas();

        let completion =
            match drive_turn(engine, &messages, &tools, &cancel, &mut on_event, started).await {
                Ok(c) => c,
                Err(e) => return Err(build_failed_turn(messages, e.partial, initial_len, e.error)),
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

        if matches!(
            completion.finish_reason,
            FinishReason::Stop | FinishReason::Length | FinishReason::ToolUse
        ) {
            auto_compress_after_response(
                engine,
                &mut messages,
                &mut metrics,
                prompt_context_tokens,
                &mut on_event,
            )
            .await;
        }

        match handle_finish_reason(
            completion,
            &mut messages,
            &ctx,
            &cancel,
            engine,
            &mut metrics,
            &mut on_event,
        )
        .await?
        {
            Some(result) => return Ok(result),
            None => continue,
        }
    }
}

/// A `drive_turn` failure: the inner `ProviderError` plus the partial
/// `Message` we managed to accumulate (None if the stream produced nothing).
pub(crate) struct DriveError {
    pub error: ProviderError,
    pub partial: Option<Message>,
}

pub(crate) async fn drive_turn(
    engine: &AgentLoop,
    messages: &[Message],
    tools: &[hermes_core::registry::ToolSchema],
    cancel: &CancellationToken,
    on_event: &mut impl FnMut(LoopEvent),
    _started: Instant,
) -> Result<Completion, DriveError> {
    on_event(LoopEvent::Thinking);
    let mut stream = engine
        .provider
        .stream(messages, tools, cancel.clone())
        .await
        .map_err(|e| DriveError {
            error: e,
            partial: None,
        })?;

    let mut acc = StreamAccumulator::new();
    let completion = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                on_event(LoopEvent::Cancelled);
                return Err(DriveError {
                    error: ProviderError::Cancelled,
                    partial: (!acc.is_empty()).then(|| acc.into_partial_message(Role::Assistant)),
                });
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
                        return Err(DriveError {
                            error: e,
                            partial: (!acc.is_empty()).then(|| acc.into_partial_message(Role::Assistant)),
                        });
                    }
                    None => break acc.finalize(),
                }
            }
        }
    };
    Ok(completion)
}

/// Returns `Ok(Some(RunResult))` for terminal finish reasons
/// (Stop / Length / ContentFilter / Error), or `Ok(None)` to continue
/// the loop after a ToolUse iteration. Provider-side errors during
/// `ToolUse` flow into `AgentRunError::FailedTurn`.
async fn handle_finish_reason(
    completion: Completion,
    messages: &mut Vec<Message>,
    ctx: &ToolContext,
    cancel: &CancellationToken,
    engine: &AgentLoop,
    metrics: &mut LoopMetrics,
    on_event: &mut impl FnMut(LoopEvent),
) -> Result<Option<RunResult>, AgentRunError> {
    match completion.finish_reason {
        FinishReason::Stop => Ok(Some(finalize(messages, completion.message, metrics))),
        FinishReason::Length => {
            on_event(LoopEvent::LengthLimit);
            Ok(Some(finalize(messages, completion.message, metrics)))
        }
        FinishReason::ContentFilter => Err(AgentRunError::Loop(LoopError::ContentFilter)),
        FinishReason::Error => Err(AgentRunError::Loop(LoopError::Provider(
            ProviderError::Other("provider returned finish_reason=error".into()),
        ))),
        FinishReason::ToolUse => {
            dispatch_tool_calls(completion, messages, ctx, cancel, engine, metrics, on_event)
                .await?;
            Ok(None)
        }
    }
}

async fn dispatch_tool_calls(
    completion: Completion,
    messages: &mut Vec<Message>,
    ctx: &ToolContext,
    cancel: &CancellationToken,
    engine: &AgentLoop,
    metrics: &mut LoopMetrics,
    on_event: &mut impl FnMut(LoopEvent),
) -> Result<(), AgentRunError> {
    let calls = completion.message.tool_calls.clone().unwrap_or_default();
    if calls.is_empty() {
        return Err(AgentRunError::Loop(LoopError::Provider(
            ProviderError::InvalidResponse("finish_reason=tool_use but no tool_calls".into()),
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

        let result = dispatch_tool(engine, &call, ctx, cancel.clone()).await;
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
    Ok(())
}

async fn dispatch_tool(
    engine: &AgentLoop,
    call: &ToolCall,
    ctx: &ToolContext,
    cancel: CancellationToken,
) -> Result<hermes_core::tool::ToolOutput, ToolError> {
    let tool = engine
        .registry
        .get(&call.name)
        .ok_or_else(|| ToolError::NotFound(call.name.clone()))?;

    // `e` is a borrowed error and `e.to_string()` allocates a new
    // String from it. There's no clone to remove — clippy flags this
    // as `redundant_clone` because the borrowed `e` is dropped right
    // after `to_string()` returns, but `to_string()` is exactly the
    // conversion we want.
    #[allow(clippy::redundant_clone)]
    let args = validate_args(&call.arguments, &tool.parameters_schema())
        .map_err(|e| ToolError::InvalidArgs(e.to_string()))?;

    tool.execute(args, ctx.clone(), cancel).await
}

fn finalize(messages: &[Message], final_msg: Message, metrics: &mut LoopMetrics) -> RunResult {
    let started_at = metrics.duration;
    let _ = started_at; // duration is updated by the caller
    RunResult {
        final_message: final_msg,
        messages: messages.to_vec(),
        metrics: metrics.clone(),
    }
}

async fn auto_compress_after_response(
    engine: &AgentLoop,
    messages: &mut Vec<Message>,
    metrics: &mut LoopMetrics,
    prompt_context_tokens: u64,
    on_event: &mut impl FnMut(LoopEvent),
) {
    let Some(engine_arc) = &engine.config.context_engine else {
        return;
    };
    let Some(context_window) = engine.config.context_window else {
        return;
    };
    if !context_window.should_compress(prompt_context_tokens) {
        return;
    }
    let should = {
        let guard = engine_arc.lock().await;
        guard.can_compress_automatically()
    };
    if !should {
        return;
    }
    if let Some(outcome) = try_compress(engine_arc, messages, None, None, false).await {
        let event = match outcome {
            CompactOutcome::Compressed { duration } => {
                metrics.compressions += 1;
                LoopEvent::CompressionCompleted {
                    trigger: CompressionTrigger::PostTurn,
                    context_tokens: Some(prompt_context_tokens),
                    duration,
                }
            }
            CompactOutcome::Skipped(reason) => LoopEvent::CompressionSkipped { reason },
            CompactOutcome::Failed { error } => LoopEvent::CompressionFailed {
                trigger: CompressionTrigger::PostTurn,
                error,
            },
        };
        on_event(event);
    }
}

/// Build an `AgentRunError::FailedTurn` (or a plain `LoopError::Provider`
/// when the provider errored before producing any message). The
/// `initial_len` check decides which path: if the agent has already
/// pushed assistant messages into history, preserving the partial
/// conversation in a `FailedTurn` is more useful than dropping it.
fn build_failed_turn(
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
