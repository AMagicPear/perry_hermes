use std::sync::{Arc, Mutex};

use hermes_agent::tools::BashTool;
use hermes_agent::{AgentLoop, AgentRunError, LoopConfig};
use hermes_core::message::{Content, Message, Role, ToolCall};
use hermes_core::provider::{Completion, FinishReason};
use hermes_core::registry::InMemoryRegistry;
use hermes_core::tool::ToolContext;
use tokio_util::sync::CancellationToken;

mod support;
use support::{ScriptedProvider, ScriptedStep};

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
    let first = Completion {
        message: Message {
            role: Role::Assistant,
            content: Content::Text(String::new()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: Some(vec![tool_call(
                "call_1",
                "terminal",
                serde_json::json!({ "command": "echo from-bash" }),
            )]),
        },
        usage: hermes_core::Usage::default(),
        finish_reason: FinishReason::ToolUse,
    };
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
        permissions: hermes_core::tool::ToolPermissions { subprocess: true },
    };

    let result = loop_
        .run(
            vec![user_message("please run something")],
            ctx,
            Arc::new(hermes_agent::SessionState::default()),
            CancellationToken::new(),
            |e| {
                events_for_cb.lock().unwrap().push(format!("{e:?}"));
            },
        )
        .await
        .expect("loop should succeed");

    assert_eq!(result.metrics.iterations, 2);
    assert_eq!(result.metrics.tool_calls, 1);
    let Content::Text(final_text) = result.final_message.content else {
        panic!("expected text")
    };
    assert_eq!(final_text, "done");

    assert_eq!(result.messages.len(), 4);
    assert_eq!(result.messages[0].role, Role::User);
    assert_eq!(result.messages[1].role, Role::Assistant);
    assert!(result.messages[1].tool_calls.is_some());
    assert_eq!(result.messages[2].role, Role::Tool);
    let Content::Text(tool_content) = &result.messages[2].content else {
        panic!("tool result should be text")
    };
    assert!(tool_content.contains("from-bash"));
    assert_eq!(result.messages[2].tool_call_id.as_deref(), Some("call_1"));
    assert_eq!(result.messages[3].role, Role::Assistant);

    let evs = events.lock().unwrap();
    assert!(evs.iter().any(|e| e.contains("ToolCallStarted")));
    assert!(evs.iter().any(|e| e.contains("ToolCallFinished")));
}

#[tokio::test]
async fn loop_routes_read_file_tool_call() {
    use hermes_agent::tools::ReadFileTool;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("dispatch.txt");
    std::fs::write(&path, "routed\n").unwrap();

    let first = Completion {
        message: Message {
            role: Role::Assistant,
            content: Content::Text(String::new()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: Some(vec![tool_call(
                "call_1",
                "read_file",
                serde_json::json!({ "path": path.to_str().unwrap() }),
            )]),
        },
        usage: hermes_core::Usage::default(),
        finish_reason: FinishReason::ToolUse,
    };
    let second = Completion {
        message: assistant_text("ok"),
        usage: hermes_core::Usage::default(),
        finish_reason: FinishReason::Stop,
    };

    let provider = ScriptedProvider::new(vec![first, second]);
    let registry = Arc::new(InMemoryRegistry::new().register(Arc::new(ReadFileTool::new())));
    let loop_ = AgentLoop::new(
        provider,
        registry,
        LoopConfig {
            max_iterations: 3,
            ..Default::default()
        },
    );
    let ctx = ToolContext {
        session_id: "test".into(),
        working_dir: dir.path().to_path_buf(),
        permissions: hermes_core::tool::ToolPermissions { subprocess: false },
    };
    let result = loop_
        .run(
            vec![user_message("read it")],
            ctx,
            Arc::new(hermes_agent::SessionState::default()),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .expect("loop should succeed");
    let Content::Text(tool_content) = &result.messages[2].content else {
        panic!("tool result should be text")
    };
    assert!(tool_content.contains("routed"));
    assert!(tool_content.contains("1|"));
}

#[tokio::test]
async fn loop_returns_partial_history_when_followup_provider_call_fails() {
    let first = Completion {
        message: Message {
            role: Role::Assistant,
            content: Content::Text(String::new()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: Some(vec![tool_call(
                "call_1",
                "terminal",
                serde_json::json!({ "command": "echo retained-output" }),
            )]),
        },
        usage: hermes_core::Usage::default(),
        finish_reason: FinishReason::ToolUse,
    };

    let provider = ScriptedProvider::from_steps(vec![
        ScriptedStep::Deltas(support::completion_to_deltas(&first)),
        ScriptedStep::Error(hermes_core::ProviderError::InvalidResponse(
            "context window exceeds limit".into(),
        )),
    ]);
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
        permissions: hermes_core::tool::ToolPermissions { subprocess: true },
    };

    let err = loop_
        .run(
            vec![user_message("please run something")],
            ctx,
            Arc::new(hermes_agent::SessionState::default()),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .expect_err("loop should surface provider failure with partial history");

    match err {
        AgentRunError::FailedTurn {
            failed_turn,
            source,
        } => {
            let messages = failed_turn.messages;
            assert!(matches!(
                source,
                hermes_core::ProviderError::InvalidResponse(_)
            ));
            assert_eq!(messages.len(), 4);
            assert_eq!(messages[0].role, Role::User);
            assert_eq!(messages[1].role, Role::Assistant);
            assert_eq!(messages[2].role, Role::Tool);
            let Content::Text(tool_content) = &messages[2].content else {
                panic!("tool result should be text")
            };
            assert!(tool_content.contains("retained-output"));
            assert_eq!(messages[3].role, Role::Assistant);
            let Content::Text(error_text) = &messages[3].content else {
                panic!("synthetic error should be text")
            };
            assert!(
                error_text.contains("Turn interrupted by error: provider error: invalid response: context window exceeds limit")
            );
        }
        other => panic!("expected FailedTurn, got {other:?}"),
    }
}

#[tokio::test]
async fn loop_keeps_partial_streamed_assistant_text_on_provider_failure() {
    use hermes_core::provider::CompletionDelta;

    let provider = ScriptedProvider::from_steps(vec![ScriptedStep::DeltasThenError(
        vec![CompletionDelta {
            content_delta: Some("partial answer".into()),
            reasoning_delta: None,
            tool_call_delta: None,
            usage: None,
            finish_reason: None,
        }],
        hermes_core::ProviderError::Transport("stream dropped".into()),
    )]);
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
        permissions: hermes_core::tool::ToolPermissions { subprocess: true },
    };

    let err = loop_
        .run(
            vec![user_message("say something")],
            ctx,
            Arc::new(hermes_agent::SessionState::default()),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .expect_err("loop should keep partial streamed text on provider failure");

    match err {
        AgentRunError::FailedTurn {
            failed_turn,
            source,
        } => {
            let messages = failed_turn.messages;
            assert!(matches!(source, hermes_core::ProviderError::Transport(_)));
            assert_eq!(messages.len(), 3);
            assert_eq!(messages[0].role, Role::User);
            assert_eq!(messages[1].role, Role::Assistant);
            let Content::Text(assistant_text) = &messages[1].content else {
                panic!("assistant partial should be text")
            };
            assert_eq!(assistant_text, "partial answer");
            assert_eq!(messages[2].role, Role::Assistant);
            let Content::Text(error_text) = &messages[2].content else {
                panic!("synthetic error should be text")
            };
            assert!(error_text.contains(
                "Turn interrupted by error: provider error: transport error: stream dropped"
            ));
        }
        other => panic!("expected FailedTurn, got {other:?}"),
    }
}
