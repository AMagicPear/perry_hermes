use std::sync::{Arc, Mutex};

use hermes_agent::{
    AIAgent, AgentLoop, CompressorConfig, ContextCompressor, HermesConfig, LoopConfig,
    ProviderConfig, ProviderKind, SessionContext,
};
use hermes_core::message::{Content, Message, Role, ToolCall};
use hermes_core::provider::{Completion, FinishReason};
use hermes_core::registry::InMemoryRegistry;
use hermes_core::tool::ToolContext;
use hermes_core::ProviderError;
use hermes_core::Usage;
use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;

mod support;
use support::ScriptedProvider;

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: Content::Text(text.into()),
        reasoning: None,
        tool_call_id: None,
        tool_calls: None,
    }
}

fn system_message(text: &str) -> Message {
    Message {
        role: Role::System,
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

fn test_ctx() -> ToolContext {
    ToolContext {
        session_id: "test".into(),
        working_dir: std::env::current_dir().unwrap_or_default(),
        permissions: hermes_core::tool::ToolPermissions { subprocess: true },
    }
}

fn test_session() -> SessionContext {
    SessionContext {
        working_dir: std::env::current_dir().unwrap_or_default(),
        session_id: "test".into(),
    }
}

#[tokio::test]
async fn loop_emits_post_turn_compression_before_tool_dispatch() {
    let first = Completion {
        message: Message {
            role: Role::Assistant,
            content: Content::Text(String::new()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: Some(vec![tool_call(
                "call_1",
                "terminal",
                serde_json::json!({ "command": "echo hi" }),
            )]),
        },
        usage: Usage {
            input_tokens: 90_000,
            output_tokens: 10,
            cached_input_tokens: 0,
        },
        finish_reason: FinishReason::ToolUse,
    };
    let second = Completion {
        message: assistant_text("done"),
        usage: Usage::default(),
        finish_reason: FinishReason::Stop,
    };

    let summary_provider = Arc::new(ScriptedProvider::new(vec![Completion {
        message: assistant_text("summary text"),
        usage: Usage::default(),
        finish_reason: FinishReason::Stop,
    }]));

    let compressor = ContextCompressor::new(CompressorConfig::default(), "test".into(), None)
        .with_summary_provider(summary_provider);

    let loop_ = AgentLoop::new(
        ScriptedProvider::new(vec![first, second]),
        Arc::new(InMemoryRegistry::new()),
        LoopConfig {
            max_iterations: 5,
            context_engine: Some(Arc::new(TokioMutex::new(compressor))),
            ..Default::default()
        },
    );

    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let events_for_cb = Arc::clone(&events);

    let _ = loop_
        .run(
            vec![
                system_message("system"),
                user_message(&"A".repeat(160_000)),
                user_message("latest question"),
            ],
            test_ctx(),
            CancellationToken::new(),
            |event| events_for_cb.lock().unwrap().push(format!("{event:?}")),
        )
        .await;

    let events = events.lock().unwrap();
    let compression_idx = events
        .iter()
        .position(|e| e.contains("CompressionCompleted"))
        .expect("expected compression event");
    let tool_idx = events
        .iter()
        .position(|e| e.contains("ToolCallStarted"))
        .expect("expected tool call event");
    assert!(
        compression_idx < tool_idx,
        "compression should happen before tool dispatch, got events: {events:?}"
    );
}

#[tokio::test]
async fn manual_compact_rewrites_history_with_summary_message() {
    let agent = AIAgent::new(
        ScriptedProvider::new(vec![Completion {
            message: assistant_text("condensed"),
            usage: Usage::default(),
            finish_reason: FinishReason::Stop,
        }]),
        HermesConfig {
            provider: ProviderConfig {
                kind: ProviderKind::Echo,
                ..Default::default()
            },
            agent: hermes_agent::AgentConfig {
                context_compression_enabled: true,
                ..Default::default()
            },
        },
    );

    let history = vec![
        system_message("system"),
        user_message("first request"),
        assistant_text("first answer"),
        user_message("second request"),
        assistant_text("second answer"),
        user_message(&"B".repeat(180_000)),
        assistant_text("large answer"),
        user_message("follow-up request"),
        assistant_text("follow-up answer"),
        user_message("latest request"),
    ];

    let (result, event) = agent
        .run_compact(history, Some("task-X"), &test_session())
        .await
        .expect("manual compact should succeed");
    match event {
        hermes_agent::LoopEvent::CompressionCompleted { .. } => {}
        other => panic!("expected CompressionCompleted event, got {other:?}"),
    }

    assert!(
        result
            .iter()
            .any(|m| matches!(&m.content, Content::Text(t) if t.contains("[CONTEXT SUMMARY"))),
        "manual compaction should inject a summary message"
    );
}

#[tokio::test]
async fn manual_compact_reports_summary_failure() {
    let agent = AIAgent::new(
        ScriptedProvider::from_steps(vec![
            support::ScriptedStep::Error(ProviderError::Transport("summary provider down".into())),
            support::ScriptedStep::Error(ProviderError::Transport(
                "summary provider still down".into(),
            )),
        ]),
        HermesConfig {
            provider: ProviderConfig {
                kind: ProviderKind::Echo,
                ..Default::default()
            },
            agent: hermes_agent::AgentConfig {
                context_compression_enabled: true,
                ..Default::default()
            },
        },
    );

    let history = vec![
        system_message("system"),
        user_message("first request"),
        assistant_text("first answer"),
        user_message("second request"),
        assistant_text("second answer"),
        user_message(&"B".repeat(180_000)),
        assistant_text("large answer"),
        user_message("follow-up request"),
        assistant_text("follow-up answer"),
        user_message("latest request"),
    ];

    let (_, event) = agent
        .run_compact(history, Some("task-X"), &test_session())
        .await
        .expect("manual compact should return a failure event, not a hard error");

    match event {
        hermes_agent::LoopEvent::CompressionFailed { error, .. } => {
            assert!(
                error.contains("summary failed") || error.contains("summary provider down"),
                "unexpected error text: {error}"
            );
        }
        other => panic!("expected CompressionFailed event, got {other:?}"),
    }
}

#[tokio::test]
async fn manual_compact_compresses_medium_history_instead_of_skipping() {
    let agent = AIAgent::new(
        ScriptedProvider::new(vec![Completion {
            message: assistant_text("condensed"),
            usage: Usage::default(),
            finish_reason: FinishReason::Stop,
        }]),
        HermesConfig {
            provider: ProviderConfig {
                kind: ProviderKind::Echo,
                ..Default::default()
            },
            agent: hermes_agent::AgentConfig {
                context_compression_enabled: true,
                ..Default::default()
            },
        },
    );

    let history = vec![
        system_message("system"),
        user_message("first request"),
        assistant_text("first answer"),
        user_message("second request"),
        assistant_text("second answer"),
        user_message("third request"),
        assistant_text("third answer"),
        user_message("follow-up request"),
        assistant_text("follow-up answer"),
        user_message("latest request"),
    ];

    let (result, event) = agent
        .run_compact(history, Some("latest request"), &test_session())
        .await
        .expect("manual compact should succeed");

    match event {
        hermes_agent::LoopEvent::CompressionCompleted { .. } => {}
        other => panic!("expected CompressionCompleted event, got {other:?}"),
    }

    assert!(
        result
            .iter()
            .any(|m| matches!(&m.content, Content::Text(t) if t.contains("condensed"))),
        "manual compaction should keep the generated summary"
    );
}
