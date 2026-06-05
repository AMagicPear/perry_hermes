//! Tests for tool argument validation inside the agent loop.
//!
//! Phase 3 minimum: a tool call with missing or wrong-typed required
//! fields must be turned into a `role: tool` error message, and the
//! loop must continue to the next provider call. This is the
//! difference between an agent and a chatbot — agents survive
//! malformed tool calls.

use std::sync::Arc;

use hermes_core::message::{Content, Message, Role, ToolCall};
use hermes_core::provider::{Completion, FinishReason};
use hermes_core::registry::InMemoryRegistry;
use hermes_core::tool::ToolContext;
use hermes_loop::{AgentLoop, LoopConfig};
use hermes_tools::BashTool;
use tokio_util::sync::CancellationToken;

mod support;
use support::ScriptedProvider;

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
        permissions: hermes_core::tool::ToolPermissions {
            subprocess: true,
            ..Default::default()
        },
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
