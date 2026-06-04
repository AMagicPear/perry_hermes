//! Phase 1 smoke test: the loop runs once against a mock provider that
//! always returns `Stop` with the user's last message echoed back, and
//! returns a `RunResult` whose metrics / final message match what we sent
//! in.
//!
//! See `plans/rust-port-design.md` §7.20 for the original spec.

use std::sync::Arc;

use hermes_core::message::{Content, Message, Role};
use hermes_core::registry::InMemoryRegistry;
use hermes_core::tool::ToolContext;
use hermes_loop::{AgentLoop, LoopConfig};
use hermes_providers::EchoProvider;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn echo_provider_runs_one_iteration_and_stops() {
    let provider = EchoProvider::new();
    let registry = Arc::new(InMemoryRegistry::new());
    let loop_ = AgentLoop::new(
        provider,
        registry,
        LoopConfig {
            max_iterations: 5,
            ..Default::default()
        },
    );

    let messages = vec![Message {
        role: Role::User,
        content: Content::Text("hello".into()),
        reasoning: None,
        tool_call_id: None,
        tool_calls: None,
    }];

    let cancel = CancellationToken::new();
    let events = std::sync::Mutex::new(Vec::new());
    let ctx = ToolContext {
        session_id: "test".into(),
        working_dir: std::env::current_dir().unwrap_or_default(),
        permissions: Default::default(),
    };

    let result = loop_
        .run(messages, ctx, cancel, |e| {
            events.lock().unwrap().push(format!("{e:?}"));
        })
        .await
        .unwrap();

    assert_eq!(result.metrics.iterations, 1);
    assert_eq!(result.metrics.tool_calls, 0);
    let final_text = match result.final_message.content {
        Content::Text(s) => s,
        _ => panic!("expected text content"),
    };
    assert_eq!(final_text, "echo: hello");
}
