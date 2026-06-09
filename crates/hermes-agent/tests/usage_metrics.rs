use std::sync::Arc;

use perry_hermes_agent::{AgentLoop, AgentSession, LoopConfig, LoopEvent};
use perry_hermes_core::message::{Content, Message, Role, ToolCall};
use perry_hermes_core::provider::{Completion, CompletionDelta, FinishReason};
use perry_hermes_core::registry::InMemoryRegistry;
use perry_hermes_core::tool::ToolContext;
use tokio_util::sync::CancellationToken;

mod support;
use support::ScriptedProvider;

fn test_session() -> AgentSession {
    AgentSession::new("test", std::env::current_dir().unwrap_or_default(), None)
}

#[tokio::test]
async fn loop_keeps_reading_after_finish_reason_to_capture_usage() {
    let provider = ScriptedProvider::from_deltas(vec![vec![
        CompletionDelta {
            content_delta: Some("done".into()),
            reasoning_delta: None,
            tool_call_delta: None,
            usage: None,
            finish_reason: None,
        },
        CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: None,
            usage: None,
            finish_reason: Some(FinishReason::Stop),
        },
        CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: None,
            usage: Some(perry_hermes_core::Usage {
                input_tokens: 12,
                output_tokens: 4,
                cached_input_tokens: 0,
            }),
            finish_reason: None,
        },
    ]]);
    let loop_ = AgentLoop::new(
        Arc::new(provider),
        Arc::new(InMemoryRegistry::new()),
        LoopConfig {
            max_iterations: 5,
            ..Default::default()
        },
    );

    let session = test_session();
    let result = loop_
        .run(
            vec![Message {
                role: Role::User,
                content: Content::Text("hello".into()),
                reasoning: None,
                tool_call_id: None,
                tool_calls: None,
            }],
            ToolContext {
                session_id: "test".into(),
                working_dir: std::env::current_dir().unwrap_or_default(),
                permissions: Default::default(),
            },
            &session,
            CancellationToken::new(),
            |_| {},
        )
        .await
        .expect("loop should succeed");

    assert_eq!(result.metrics.iterations, 1);
    assert_eq!(result.metrics.input_tokens, 12);
    assert_eq!(result.metrics.output_tokens, 4);
}

#[tokio::test]
async fn context_usage_includes_cached_provider_input_tokens_mid_tool_loop() {
    let provider = ScriptedProvider::new(vec![
        Completion {
            message: Message {
                role: Role::Assistant,
                content: Content::Text(String::new()),
                reasoning: None,
                tool_call_id: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".into(),
                    name: "terminal".into(),
                    arguments: serde_json::json!({ "command": "true" }),
                }]),
            },
            usage: perry_hermes_core::Usage {
                input_tokens: 30,
                output_tokens: 1,
                cached_input_tokens: 7_000,
            },
            finish_reason: FinishReason::ToolUse,
        },
        Completion {
            message: Message {
                role: Role::Assistant,
                content: Content::Text("done".into()),
                reasoning: None,
                tool_call_id: None,
                tool_calls: None,
            },
            usage: perry_hermes_core::Usage {
                input_tokens: 7_400,
                output_tokens: 1,
                cached_input_tokens: 0,
            },
            finish_reason: FinishReason::Stop,
        },
    ]);
    let loop_ = AgentLoop::new(
        Arc::new(provider),
        Arc::new(
            InMemoryRegistry::new()
                .register(Arc::new(perry_hermes_skill_tools::tools::BashTool::new())),
        ),
        LoopConfig {
            max_iterations: 5,
            system_prompt: Some("z".repeat(28_000)),
            ..Default::default()
        },
    );

    let session = test_session();
    let mut usage_events = Vec::new();
    let result = loop_
        .run(
            vec![Message {
                role: Role::User,
                content: Content::Text("hello".into()),
                reasoning: None,
                tool_call_id: None,
                tool_calls: None,
            }],
            ToolContext {
                session_id: "test".into(),
                working_dir: std::env::current_dir().unwrap_or_default(),
                permissions: perry_hermes_core::tool::ToolPermissions { subprocess: true },
            },
            &session,
            CancellationToken::new(),
            |ev| {
                if let LoopEvent::ContextUsageUpdated { used_tokens } = ev {
                    usage_events.push(used_tokens);
                }
            },
        )
        .await
        .expect("loop should succeed");

    assert_eq!(result.metrics.iterations, 2);
    assert!(
        usage_events.contains(&7_030),
        "expected cached prompt tokens to be included in context usage, got {usage_events:?}"
    );
    assert!(
        !usage_events.contains(&30),
        "display context usage must not use bare Anthropic input_tokens when cache tokens exist: {usage_events:?}"
    );
}

#[tokio::test]
async fn loop_emits_context_usage_only_from_normalized_real_usage() {
    let provider = ScriptedProvider::from_deltas(vec![vec![
        CompletionDelta {
            content_delta: Some("done".into()),
            reasoning_delta: None,
            tool_call_delta: None,
            usage: None,
            finish_reason: None,
        },
        CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: None,
            usage: Some(perry_hermes_core::Usage {
                input_tokens: 42,
                output_tokens: 7,
                cached_input_tokens: 1_000,
            }),
            finish_reason: Some(FinishReason::Stop),
        },
    ]]);
    let loop_ = AgentLoop::new(
        Arc::new(provider),
        Arc::new(InMemoryRegistry::new()),
        LoopConfig {
            max_iterations: 5,
            system_prompt: Some("system prompt included in real request".into()),
            ..Default::default()
        },
    );

    let session = test_session();
    let mut usage_events = Vec::new();
    let result = loop_
        .run(
            vec![Message {
                role: Role::User,
                content: Content::Text("hello".into()),
                reasoning: None,
                tool_call_id: None,
                tool_calls: None,
            }],
            ToolContext {
                session_id: "test".into(),
                working_dir: std::env::current_dir().unwrap_or_default(),
                permissions: Default::default(),
            },
            &session,
            CancellationToken::new(),
            |ev| {
                if let LoopEvent::ContextUsageUpdated { used_tokens } = ev {
                    usage_events.push(used_tokens);
                }
            },
        )
        .await
        .expect("loop should succeed");

    assert_eq!(result.metrics.input_tokens, 42);
    assert_eq!(
        usage_events,
        vec![1_042],
        "context usage must come only from provider-reported prompt tokens"
    );
    assert!(
        !usage_events.contains(&42),
        "display context usage must not use bare input_tokens when cache tokens exist: {usage_events:?}"
    );
}
