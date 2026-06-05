use hermes_agent::{AgentLoop, LoopConfig};
use hermes_providers::EchoProvider;
use std::sync::Arc;

#[tokio::test]
async fn echo_provider_runs_one_iteration_and_stops() {
    let provider = EchoProvider::new();
    let registry = Arc::new(hermes_core::InMemoryRegistry::new());
    let loop_ = AgentLoop::new(
        provider,
        registry,
        LoopConfig {
            max_iterations: 2,
            ..Default::default()
        },
    );

    let result = loop_
        .run(
            vec![hermes_core::Message {
                role: hermes_core::Role::User,
                content: hermes_core::Content::Text("hello".into()),
                reasoning: None,
                tool_call_id: None,
                tool_calls: None,
            }],
            hermes_core::ToolContext {
                session_id: "test".into(),
                working_dir: std::env::current_dir().unwrap(),
                permissions: hermes_core::ToolPermissions { subprocess: false },
            },
            tokio_util::sync::CancellationToken::new(),
            |_| {},
        )
        .await
        .expect("loop should succeed");

    assert_eq!(result.metrics.iterations, 1);
}
