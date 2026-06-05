//! Tests for tool argument validation inside the agent loop.
//!
//! Phase 3 minimum: a tool call with missing or wrong-typed required
//! fields must be turned into a `role: tool` error message, and the
//! loop must continue to the next provider call. This is the
//! difference between an agent and a chatbot — agents survive
//! malformed tool calls.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use hermes_core::message::{Content, Message, Role, ToolCall};
use hermes_core::provider::{Completion, CompletionDelta, CompletionStream, FinishReason, Provider, ToolCallDelta};
use hermes_core::registry::{InMemoryRegistry, ToolSchema};
use hermes_core::tool::ToolContext;
use hermes_core::ProviderError;
use hermes_loop::{AgentLoop, LoopConfig};
use hermes_tools::BashTool;
use tokio_util::sync::CancellationToken;

struct ScriptedProvider {
    // The script is a list of (Vec<CompletionDelta>) — one inner vec per
    // call to `stream()`. The default `complete()` impl drives each
    // scripted stream through `accumulate_stream` to produce a Completion.
    script: Mutex<Vec<Vec<CompletionDelta>>>,
    #[allow(dead_code)]
    call_count: AtomicUsize,
}

impl ScriptedProvider {
    fn new(script: Vec<Completion>) -> Self {
        // Convert each scripted Completion into the equivalent sequence of
        // deltas the loop would see if the provider were streaming.
        let script: Vec<Vec<CompletionDelta>> = script
            .into_iter()
            .map(completion_to_deltas)
            .collect();
        Self {
            script: Mutex::new(script),
            call_count: AtomicUsize::new(0),
        }
    }
}

fn completion_to_deltas(c: hermes_core::provider::Completion) -> Vec<CompletionDelta> {
    // Mirror the structure StreamAccumulator::add expects.
    let mut deltas = Vec::new();
    // Content + reasoning (if any) on the first delta.
    let has_text = matches!(&c.message.content, Content::Text(t) if !t.is_empty());
    let has_reasoning = c.message.reasoning.as_ref().map(|s| !s.is_empty()).unwrap_or(false);
    if has_text || has_reasoning {
        let text = match &c.message.content {
            Content::Text(t) => Some(t.clone()),
            _ => None,
        };
        deltas.push(CompletionDelta {
            content_delta: text,
            reasoning_delta: c.message.reasoning.clone(),
            tool_call_delta: None,
            usage: Some(c.usage),
            finish_reason: None,
        });
    }
    // Tool calls: one delta per call, with id+name+arguments in the first chunk.
    if let Some(calls) = &c.message.tool_calls {
        for (i, tc) in calls.iter().enumerate() {
            deltas.push(CompletionDelta {
                content_delta: None,
                reasoning_delta: None,
                tool_call_delta: Some(ToolCallDelta {
                    index: i,
                    id: Some(tc.id.clone()),
                    name: Some(tc.name.clone()),
                    arguments_delta: Some(tc.arguments.to_string()),
                }),
                usage: None,
                finish_reason: None,
            });
        }
    }
    // Final delta carries the finish_reason (and usage if not already carried).
    deltas.push(CompletionDelta {
        content_delta: None,
        reasoning_delta: None,
        tool_call_delta: None,
        usage: Some(c.usage),
        finish_reason: Some(c.finish_reason),
    });
    deltas
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn name(&self) -> &str {
        "scripted"
    }
    fn model(&self) -> &str {
        "scripted-v0"
    }
    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _cancel: CancellationToken,
    ) -> Result<CompletionStream, ProviderError> {
        let mut script = self.script.lock().unwrap();
        if script.is_empty() {
            panic!("ScriptedProvider: script exhausted — the loop called stream() more times than scripted");
        }
        let deltas = script.remove(0);
        self.call_count.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(stream::iter(deltas.into_iter().map(Ok))))
    }
}

#[tokio::test]
async fn loop_turns_invalid_tool_args_into_tool_error_message_and_continues() {
    // 1st call: LLM emits a tool call with no `command` field — invalid.
    let first = Completion {
        message: Message {
            role: Role::Assistant,
            content: Content::Text(String::new()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_bad".into(),
                name: "bash".into(),
                // missing required "command" field
                arguments: serde_json::json!({}),
            }]),
        },
        usage: hermes_core::Usage::default(),
        finish_reason: FinishReason::ToolUse,
    };
    // 2nd call: LLM reacts to the error and gives a final answer.
    let second = Completion {
        message: Message {
            role: Role::Assistant,
            content: Content::Text("I see, I should have provided a command".into()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        },
        usage: hermes_core::Usage::default(),
        finish_reason: FinishReason::Stop,
    };

    let provider = ScriptedProvider::new(vec![first, second]);
    let registry = Arc::new(InMemoryRegistry::new().register(Arc::new(BashTool::new())));
    let loop_ = AgentLoop::new(
        provider,
        registry,
        LoopConfig {
            max_iterations: 5,
            ..Default::default()
        },
    );

    let ctx = ToolContext {
        session_id: "test".into(),
        working_dir: std::env::current_dir().unwrap_or_default(),
        permissions: Default::default(),
    };

    let result = loop_
        .run(
            vec![Message {
                role: Role::User,
                content: Content::Text("try a tool call".into()),
                reasoning: None,
                tool_call_id: None,
                tool_calls: None,
            }],
            ctx,
            CancellationToken::new(),
            |_| {},
        )
        .await
        .expect("loop should survive invalid tool args");

    // Two iterations, one tool call (the bad one), final text from second call.
    assert_eq!(result.metrics.iterations, 2);
    assert_eq!(result.metrics.tool_calls, 1);

    // Trajectory: user, assistant(tool_call), tool(error), assistant(final)
    assert_eq!(result.messages.len(), 4);
    assert_eq!(result.messages[2].role, Role::Tool);
    let err_content = match &result.messages[2].content {
        Content::Text(s) => s.clone(),
        _ => panic!("tool result should be text"),
    };
    assert!(
        err_content.contains("Error"),
        "expected 'Error' prefix, got: {err_content}"
    );
    assert!(
        err_content.to_lowercase().contains("command"),
        "expected error to mention the missing field, got: {err_content}"
    );
    assert_eq!(result.messages[2].tool_call_id.as_deref(), Some("call_bad"));
}
