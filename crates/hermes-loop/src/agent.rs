//! The agent loop — calls the LLM, reacts to `finish_reason`, dispatches
//! tools, returns a `RunResult`.
//!
//! State machine (see `plans/rust-port-design.md` §1 and §4):
//!
//!   loop {
//!     check cancel / budget / iteration limit
//!     ask the provider
//!     match finish_reason:
//!       Stop / Length          → return
//!       ContentFilter / Error  → return Err
//!       ToolUse                → for each call: validate args, dispatch
//!                                tool, append `role: tool` message,
//!                                continue loop
//!   }
//!
//! Tools errors are NOT fatal — they become `role: tool` content
//! "Error: …" messages so the LLM can see what went wrong and choose
//! to retry or pivot. This is the key design decision that makes the
//! agent robust to flaky tools (network blips, transient FS errors, …).

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use hermes_core::error::{LoopError, ProviderError};
use hermes_core::message::{Content, Message, Role, ToolCall};
use hermes_core::provider::{FinishReason, Provider, ToolCallDelta};
use hermes_core::registry::ToolRegistry;
use hermes_core::tool::{ToolContext, ToolOutput};

/// The agent loop. Generic over `P: Provider` and `R: ToolRegistry` so
/// tests can swap in mocks. The loop holds a reference to the registry
/// (not ownership) so multiple loops can share it.
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
    /// Optional system prompt prepended to messages if not already
    /// present.
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
    /// One text token from a streaming assistant message.
    ContentDelta(String),
    /// One reasoning token (o1, extended thinking).
    ReasoningDelta(String),
    /// A delta for a streaming tool call. Silent-accumulated by the loop;
    /// `ToolCallStarted` fires only when the call is complete.
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

impl<P: Provider, R: ToolRegistry> AgentLoop<P, R> {
    pub fn new(provider: P, registry: Arc<R>, config: LoopConfig) -> Self {
        Self {
            provider,
            registry,
            config,
        }
    }

    /// Run a full conversation. May iterate multiple times — each
    /// `ToolUse` finish reason dispatches the tool, appends a
    /// `role: tool` result, and asks the provider again.
    pub async fn run(
        &self,
        initial_messages: Vec<Message>,
        ctx: ToolContext,
        cancel: CancellationToken,
        mut on_event: impl FnMut(LoopEvent) + Send,
    ) -> Result<RunResult, LoopError> {
        let mut messages = initial_messages;
        let mut metrics = LoopMetrics::default();
        let started = Instant::now();

        // If a system prompt is configured, inject it at the front
        // unless the user already supplied one.
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
            // ── 1. Exit checks ─────────────────────────────────────
            if cancel.is_cancelled() {
                on_event(LoopEvent::Cancelled);
                return Err(LoopError::Cancelled);
            }
            if metrics.iterations >= self.config.max_iterations {
                on_event(LoopEvent::IterationsExhausted);
                return Err(LoopError::MaxIterations(metrics.iterations));
            }
            if started.elapsed() > self.config.max_duration {
                return Err(LoopError::Timeout(started.elapsed()));
            }

            // ── 2. Resolve tool schemas ────────────────────────────
            let tools = self.registry.schemas();

            // ── 3. Call the LLM (streaming) ────────────────────────
            on_event(LoopEvent::Thinking);
            let mut stream = self
                .provider
                .stream(&messages, &tools, cancel.clone())
                .await?;
            let mut acc = hermes_core::provider::StreamAccumulator::new();
            let completion = loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        on_event(LoopEvent::Cancelled);
                        return if acc.is_empty() {
                            Err(LoopError::Cancelled)
                        } else {
                            Err(LoopError::CancelledWith(acc.into_partial_message(Role::Assistant)))
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
                            Some(Err(e)) => return Err(LoopError::Provider(e)),
                            None => break acc.finalize(),
                        }
                    }
                }
            };
            metrics.iterations += 1;
            metrics.input_tokens += completion.usage.input_tokens;
            metrics.output_tokens += completion.usage.output_tokens;

            // ── 4. Persist the assistant message ──────────────────
            let assistant_msg = completion.message.clone();
            messages.push(assistant_msg.clone());
            on_event(LoopEvent::AssistantMessage(assistant_msg.clone()));

            // ── 5. React to finish reason ──────────────────────────
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
                    return Err(LoopError::ContentFilter);
                }
                FinishReason::Error => {
                    return Err(LoopError::Provider(ProviderError::Other(
                        "provider returned finish_reason=error".into(),
                    )));
                }
                FinishReason::ToolUse => {
                    let calls = assistant_msg.tool_calls.clone().unwrap_or_default();

                    if calls.is_empty() {
                        // Provider said tool_use but sent no tool_calls.
                        // Bail rather than loop forever.
                        return Err(LoopError::Provider(ProviderError::InvalidResponse(
                            "finish_reason=tool_use but no tool_calls".into(),
                        )));
                    }

                    for call in calls {
                        if cancel.is_cancelled() {
                            on_event(LoopEvent::Cancelled);
                            return Err(LoopError::Cancelled);
                        }
                        on_event(LoopEvent::ToolCallStarted {
                            call: call.clone(),
                            iteration: metrics.iterations,
                        });

                        let result = self.dispatch_tool(&call, &ctx, cancel.clone()).await;

                        // Tool errors are NOT fatal — they become
                        // `role: tool` content "Error: …" messages
                        // so the LLM can see what failed and decide
                        // what to do.
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
                    // Loop continues — LLM sees the tool results and
                    // decides what's next.
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

        // Validate the LLM's arguments against the tool's JSON Schema
        // before invoking. Phase 3: always validate, no caching of
        // the compiled schema (a future phase will cache in the
        // registry).
        let args = validate_args(&call.arguments, tool.parameters_schema())
            .map_err(|e| hermes_core::error::ToolError::InvalidArgs(e.to_string()))?;

        tool.execute(args, ctx.clone(), cancel).await
    }
}

/// Validate `args` against the tool's JSON Schema (draft-07). LLM
/// output is raw JSON; the schema is the only contract. Compilation is
/// fast enough to do per-call for now; a future phase will cache the
/// compiled `JSONSchema` in the registry.
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
