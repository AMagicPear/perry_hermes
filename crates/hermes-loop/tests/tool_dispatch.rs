//! Integration tests for tool dispatch inside the agent loop.
//!
//! Phase 3 minimum: when the provider returns `FinishReason::ToolUse`,
//! the loop must
//!   1. look the tool up in the registry
//!   2. invoke it with the call's arguments
//!   3. append the result as a `role: tool` message
//!   4. call the provider again so the LLM can react
//!
//! The final assistant message should come from the *second* provider
//! call, and the metrics should reflect two iterations + one tool call.

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

/// A test provider that scripts a fixed sequence of completions, one
/// per call to `stream()`. Each script entry is converted to deltas
/// internally so the loop sees a streaming provider.
struct ScriptedProvider {
    script: Mutex<Vec<Vec<CompletionDelta>>>,
    #[allow(dead_code)]
    call_count: AtomicUsize,
}

impl ScriptedProvider {
    fn new(script: Vec<hermes_core::provider::Completion>) -> Self {
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

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: Content::Text(text.into()),
        reasoning: None,
        tool_call_id: None,
        tool_calls: None,
    }
}

fn assistant_text(text: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: Content::Text(text.into()),
        reasoning: None,
        tool_call_id: None,
        tool_calls: None,
    }
}

fn tool_call(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args,
    }
}

#[tokio::test]
async fn loop_dispatches_tool_call_and_appends_tool_result_message() {
    // ── 1st call: LLM wants to run `echo from-bash` ──
    let first = Completion {
        message: Message {
            role: Role::Assistant,
            content: Content::Text(String::new()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: Some(vec![tool_call(
                "call_1",
                "bash",
                serde_json::json!({ "command": "echo from-bash" }),
            )]),
        },
        usage: hermes_core::Usage::default(),
        finish_reason: FinishReason::ToolUse,
    };
    // ── 2nd call: LLM responds with the final answer ──
    let second = Completion {
        message: assistant_text("done"),
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

    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let events_for_cb = Arc::clone(&events);

    let ctx = ToolContext {
        session_id: "test".into(),
        working_dir: std::env::current_dir().unwrap_or_default(),
        permissions: Default::default(),
    };

    let result = loop_
        .run(vec![user_message("please run something")], ctx, CancellationToken::new(), |e| {
            events_for_cb.lock().unwrap().push(format!("{e:?}"));
        })
        .await
        .expect("loop should succeed");

    // ── Two iterations, one tool call, final answer is "done" ──
    assert_eq!(result.metrics.iterations, 2);
    assert_eq!(result.metrics.tool_calls, 1);
    let final_text = match result.final_message.content {
        Content::Text(s) => s,
        _ => panic!("expected text"),
    };
    assert_eq!(final_text, "done");

    // ── Trajectory should contain: user, assistant(tool_call), tool(result), assistant("done") ──
    assert_eq!(result.messages.len(), 4);
    assert_eq!(result.messages[0].role, Role::User);
    assert_eq!(result.messages[1].role, Role::Assistant);
    assert!(result.messages[1].tool_calls.is_some());
    assert_eq!(result.messages[2].role, Role::Tool);
    let tool_content = match &result.messages[2].content {
        Content::Text(s) => s.clone(),
        _ => panic!("tool result should be text"),
    };
    assert!(
        tool_content.contains("from-bash"),
        "tool result should contain bash output, got: {tool_content}"
    );
    assert_eq!(result.messages[2].tool_call_id.as_deref(), Some("call_1"));
    assert_eq!(result.messages[3].role, Role::Assistant);

    // ── Events: Thinking, AssistantMessage(tool), ToolCallStarted, ToolCallFinished, Thinking, AssistantMessage(done) ──
    let evs = events.lock().unwrap();
    assert!(
        evs.iter().any(|e| e.contains("ToolCallStarted")),
        "expected a ToolCallStarted event, got: {evs:?}"
    );
    assert!(
        evs.iter().any(|e| e.contains("ToolCallFinished")),
        "expected a ToolCallFinished event, got: {evs:?}"
    );
}
