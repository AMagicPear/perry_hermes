use std::sync::Arc;

use perry_hermes_agent::{AgentLoop, AgentSession, LoopConfig};
use perry_hermes_core::message::{Content, Message, Role, ToolCall};
use perry_hermes_core::provider::{Completion, FinishReason};
use perry_hermes_core::registry::InMemoryRegistry;
use perry_hermes_core::tool::ToolContext;
use perry_hermes_skill_tools::tools::BashTool;
use tokio_util::sync::CancellationToken;

mod support;
use support::ScriptedProvider;

fn test_session() -> AgentSession {
    AgentSession::new("test", std::env::current_dir().unwrap_or_default(), None)
}

#[tokio::test]
async fn loop_turns_invalid_tool_args_into_tool_error_message_and_continues() {
    let first = Completion {
        message: Message {
            role: Role::Assistant,
            content: Content::Text(String::new()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_bad".into(),
                name: "terminal".into(),
                arguments: serde_json::json!({}),
            }]),
        },
        usage: perry_hermes_core::Usage::default(),
        finish_reason: FinishReason::ToolUse,
    };
    let second = Completion {
        message: Message {
            role: Role::Assistant,
            content: Content::Text("I see, I should have provided a command".into()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        },
        usage: perry_hermes_core::Usage::default(),
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
        permissions: perry_hermes_core::tool::ToolPermissions { subprocess: true },
    };

    let session = test_session();
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
            &session,
            CancellationToken::new(),
            |_| {},
        )
        .await
        .expect("loop should survive invalid tool args");

    assert_eq!(result.metrics.iterations, 2);
    assert_eq!(result.metrics.tool_calls, 1);
    assert_eq!(result.messages.len(), 4);
    assert_eq!(result.messages[2].role, Role::Tool);
    let err_content = match &result.messages[2].content {
        Content::Text(s) => s.clone(),
        _ => panic!("tool result should be text"),
    };
    assert!(err_content.contains("Error"));
    assert!(err_content.to_lowercase().contains("command"));
    assert_eq!(result.messages[2].tool_call_id.as_deref(), Some("call_bad"));
}
