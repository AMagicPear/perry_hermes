use std::sync::{Arc, Mutex};

use hermes_agent::{
    AIAgent, AgentLoop, CompactorConfig, ContextWindow, HermesConfig, LoopConfig, ModelConfig,
    ProviderConfig, ProviderKind, SessionContext, SessionState, SummaryCompactor,
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

fn echo_config_with_compression() -> HermesConfig {
    HermesConfig {
        providers: vec![ProviderConfig {
            name: "local".into(),
            kind: ProviderKind::Echo,
            api_key_env: None,
            models: vec![ModelConfig {
                name: "echo".into(),
                context_window_size: 128_000,
            }],
            base_url: None,
            api_key_header: None,
            thinking: None,
        }],
        agent: hermes_agent::AgentConfig {
            default_provider: "local".into(),
            default_model: "echo".into(),
            context_compression_enabled: true,
            ..Default::default()
        },
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

fn test_session_state() -> Arc<SessionState> {
    Arc::new(SessionState::default())
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

    let compactor =
        SummaryCompactor::new(CompactorConfig::default()).with_summary_provider(summary_provider);

    let loop_ = AgentLoop::new(
        ScriptedProvider::new(vec![first, second]),
        Arc::new(InMemoryRegistry::new()),
        LoopConfig {
            max_iterations: 5,
            compaction_strategy: Some(Arc::new(TokioMutex::new(compactor))),
            context_window: Some(ContextWindow {
                max_tokens: 128_000,
                compression_threshold_ratio: 0.50,
            }),
            ..Default::default()
        },
    );

    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let events_for_cb = Arc::clone(&events);

    let _ = loop_
        .run(
            vec![
                system_message("system"),
                user_message("first request"),
                assistant_text("first answer"),
                user_message("second request"),
                assistant_text("second answer"),
                user_message("third request"),
                assistant_text("third answer"),
                user_message(&"A".repeat(160_000)),
                user_message("latest question"),
            ],
            test_ctx(),
            test_session_state(),
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
async fn loop_does_not_compress_until_real_context_usage_reaches_threshold() {
    let first = Completion {
        message: assistant_text("small turn"),
        usage: Usage {
            input_tokens: 63_999,
            output_tokens: 10,
            cached_input_tokens: 0,
        },
        finish_reason: FinishReason::Stop,
    };

    let summary_provider = Arc::new(ScriptedProvider::new(vec![Completion {
        message: assistant_text("summary text"),
        usage: Usage::default(),
        finish_reason: FinishReason::Stop,
    }]));

    let compactor =
        SummaryCompactor::new(CompactorConfig::default()).with_summary_provider(summary_provider);

    let loop_ = AgentLoop::new(
        ScriptedProvider::new(vec![first]),
        Arc::new(InMemoryRegistry::new()),
        LoopConfig {
            max_iterations: 5,
            compaction_strategy: Some(Arc::new(TokioMutex::new(compactor))),
            context_window: Some(ContextWindow {
                max_tokens: 128_000,
                compression_threshold_ratio: 0.50,
            }),
            ..Default::default()
        },
    );

    let mut events = Vec::new();
    loop_
        .run(
            vec![
                system_message("system"),
                user_message(&"A".repeat(200_000)),
                user_message("latest question"),
            ],
            test_ctx(),
            test_session_state(),
            CancellationToken::new(),
            |event| events.push(event),
        )
        .await
        .expect("loop should succeed");

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, hermes_agent::LoopEvent::CompressionCompleted { .. })),
        "compression must wait for real provider usage to reach threshold"
    );
}

#[tokio::test]
async fn loop_compresses_after_real_context_usage_reaches_threshold() {
    let first = Completion {
        message: assistant_text("large turn"),
        usage: Usage {
            input_tokens: 64_000,
            output_tokens: 10,
            cached_input_tokens: 0,
        },
        finish_reason: FinishReason::Stop,
    };

    let summary_provider = Arc::new(ScriptedProvider::new(vec![Completion {
        message: assistant_text("summary text"),
        usage: Usage::default(),
        finish_reason: FinishReason::Stop,
    }]));

    let compactor =
        SummaryCompactor::new(CompactorConfig::default()).with_summary_provider(summary_provider);

    let loop_ = AgentLoop::new(
        ScriptedProvider::new(vec![first]),
        Arc::new(InMemoryRegistry::new()),
        LoopConfig {
            max_iterations: 5,
            compaction_strategy: Some(Arc::new(TokioMutex::new(compactor))),
            context_window: Some(ContextWindow {
                max_tokens: 128_000,
                compression_threshold_ratio: 0.50,
            }),
            ..Default::default()
        },
    );

    let mut events = Vec::new();
    let result = loop_
        .run(
            vec![
                system_message("system"),
                user_message("first request"),
                assistant_text("first answer"),
                user_message(&"A".repeat(200_000)),
                user_message("latest question"),
            ],
            test_ctx(),
            test_session_state(),
            CancellationToken::new(),
            |event| events.push(event),
        )
        .await
        .expect("loop should succeed");

    assert!(
        events.iter().any(|event| {
            matches!(
                event,
                hermes_agent::LoopEvent::CompressionCompleted {
                    trigger: hermes_core::compaction_strategy::CompressionTrigger::PostTurn,
                    context_tokens: Some(64_000),
                    ..
                }
            )
        }),
        "expected post-response compression after real provider usage hit threshold: {events:?}"
    );
    assert!(
        result
            .messages
            .iter()
            .filter(|m| m.role == Role::User)
            .count()
            == 2,
        "compressed history should keep first user plus one summary: {:?}",
        result.messages
    );
    assert!(
        matches!(
            result.messages.as_slice(),
            [
                Message { role: Role::System, .. },
                Message { role: Role::User, content: Content::Text(first), .. },
                Message { role: Role::User, content: Content::Text(summary), .. },
            ] if first == "first request" && summary.contains("[CONTEXT SUMMARY")
        ),
        "compressed history should be system + first user + summary, got {:?}",
        result.messages
    );
}

#[tokio::test]
async fn loop_reports_post_compact_usage_from_baseline_plus_summary_output() {
    let first = Completion {
        message: assistant_text("large turn"),
        usage: Usage {
            input_tokens: 64_000,
            output_tokens: 10,
            cached_input_tokens: 0,
        },
        finish_reason: FinishReason::Stop,
    };

    let summary_provider = Arc::new(ScriptedProvider::new(vec![Completion {
        message: assistant_text("summary text"),
        usage: Usage {
            input_tokens: 9_000,
            output_tokens: 1_200,
            cached_input_tokens: 0,
        },
        finish_reason: FinishReason::Stop,
    }]));

    let compactor =
        SummaryCompactor::new(CompactorConfig::default()).with_summary_provider(summary_provider);

    let loop_ = AgentLoop::new(
        ScriptedProvider::new(vec![first]),
        Arc::new(InMemoryRegistry::new()),
        LoopConfig {
            max_iterations: 5,
            compaction_strategy: Some(Arc::new(TokioMutex::new(compactor))),
            context_window: Some(ContextWindow {
                max_tokens: 128_000,
                compression_threshold_ratio: 0.50,
            }),
            ..Default::default()
        },
    );

    let mut usage_events = Vec::new();
    loop_
        .run(
            vec![
                system_message("system"),
                user_message("first request"),
                assistant_text("first answer"),
                user_message(&"A".repeat(200_000)),
                user_message("latest question"),
            ],
            test_ctx(),
            test_session_state(),
            CancellationToken::new(),
            |event| {
                if let hermes_agent::LoopEvent::ContextUsageUpdated { used_tokens } = event {
                    usage_events.push(used_tokens);
                }
            },
        )
        .await
        .expect("loop should succeed");

    assert_eq!(
        usage_events,
        vec![64_000, 65_200],
        "post-compact usage should be first prompt baseline plus summary output"
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
        echo_config_with_compression(),
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
        matches!(
            result.as_slice(),
            [
                Message { role: Role::System, .. },
                Message { role: Role::User, content: Content::Text(first), .. },
                Message { role: Role::User, content: Content::Text(summary), .. },
            ] if first == "first request" && summary.contains("[CONTEXT SUMMARY")
        ),
        "manual compaction should keep only system + first user + summary, got {result:?}"
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
        echo_config_with_compression(),
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
        echo_config_with_compression(),
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

#[test]
fn threshold_tokens_scales_with_context_window_size() {
    let window = ContextWindow {
        max_tokens: 200_000,
        compression_threshold_ratio: 0.50,
    };
    assert_eq!(window.threshold_tokens(), 100_000);

    let window = ContextWindow {
        max_tokens: 128_000,
        compression_threshold_ratio: 0.60,
    };
    assert_eq!(window.threshold_tokens(), 76_800);
}

#[test]
fn context_window_uses_real_usage_threshold() {
    let window = ContextWindow {
        max_tokens: 128_000,
        compression_threshold_ratio: 0.50,
    };

    assert!(!window.should_compress(63_999));
    assert!(window.should_compress(64_000));
}
