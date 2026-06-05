use std::sync::Arc;

use hermes_core::message::{Content, Message, Role};
use hermes_core::provider::{CompletionDelta, FinishReason};
use hermes_core::registry::InMemoryRegistry;
use hermes_core::tool::ToolContext;
use hermes_loop::{AgentLoop, LoopConfig};
use tokio_util::sync::CancellationToken;

mod support;
use support::ScriptedProvider;

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
            usage: Some(hermes_core::Usage {
                input_tokens: 12,
                output_tokens: 4,
                cached_input_tokens: 0,
            }),
            finish_reason: None,
        },
    ]]);
    let loop_ = AgentLoop::new(
        provider,
        Arc::new(InMemoryRegistry::new()),
        LoopConfig {
            max_iterations: 5,
            ..Default::default()
        },
    );

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
            CancellationToken::new(),
            |_| {},
        )
        .await
        .expect("loop should succeed");

    assert_eq!(result.metrics.iterations, 1);
    assert_eq!(result.metrics.input_tokens, 12);
    assert_eq!(result.metrics.output_tokens, 4);
}
