use std::sync::{Arc, Mutex};

use perry_hermes_agent::{
    AIAgent, AgentLoop, AgentSession, CompactorConfig, ContextWindow, LoopConfig, ModelConfig,
    PerryHermesConfig, SummaryCompactor,
};
use perry_hermes_core::ProviderError;
use perry_hermes_core::Usage;
use perry_hermes_core::message::{Content, Message, Role, ToolCall};
use perry_hermes_core::provider::{Completion, FinishReason};
use perry_hermes_core::registry::InMemoryRegistry;
use perry_hermes_core::tool::ToolContext;
use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;

mod common;
mod support;
use support::ScriptedProvider;

fn echo_config_with_compression() -> PerryHermesConfig {
    common::for_test_echo()
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
        permissions: perry_hermes_core::tool::ToolPermissions { subprocess: true },
    }
}

fn test_session() -> AgentSession {
    AgentSession::new("test", std::env::current_dir().unwrap_or_default(), None)
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

    let session = test_session();
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
            &session,
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

    let session = test_session();
    let mut events = Vec::new();
    loop_
        .run(
            vec![
                system_message("system"),
                user_message(&"A".repeat(200_000)),
                user_message("latest question"),
            ],
            test_ctx(),
            &session,
            CancellationToken::new(),
            |event| events.push(event),
        )
        .await
        .expect("loop should succeed");

    assert!(
        !events.iter().any(|event| matches!(
            event,
            perry_hermes_agent::LoopEvent::CompressionCompleted { .. }
        )),
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

    let session = test_session();
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
            &session,
            CancellationToken::new(),
            |event| events.push(event),
        )
        .await
        .expect("loop should succeed");

    assert!(
        events.iter().any(|event| {
            matches!(
                event,
                perry_hermes_agent::LoopEvent::CompressionCompleted {
                    trigger: perry_hermes_core::compaction_strategy::CompressionTrigger::PostTurn,
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

    let session = test_session();
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
            &session,
            CancellationToken::new(),
            |event| {
                if let perry_hermes_agent::LoopEvent::ContextUsageUpdated { used_tokens } = event {
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
async fn session_compact_rewrites_history_with_summary_message() {
    let agent = AIAgent::new(
        ScriptedProvider::new(vec![Completion {
            message: assistant_text("condensed"),
            usage: Usage::default(),
            finish_reason: FinishReason::Stop,
        }]),
        echo_config_with_compression(),
    );

    let session = AgentSession::new(
        "test",
        std::env::current_dir().unwrap_or_default(),
        Some(system_message("system")),
    );
    session
        .replace_messages(vec![
            user_message("first request"),
            assistant_text("first answer"),
            user_message("second request"),
            assistant_text("second answer"),
            user_message(&"B".repeat(180_000)),
            assistant_text("large answer"),
            user_message("follow-up request"),
            assistant_text("follow-up answer"),
            user_message("latest request"),
        ])
        .await;

    let event = agent
        .compact_session(&session, Some("task-X"))
        .await
        .expect("manual compact should succeed");
    match event {
        perry_hermes_agent::LoopEvent::CompressionCompleted { .. } => {}
        other => panic!("expected CompressionCompleted event, got {other:?}"),
    }
    // After manual compaction:
    //   * `messages` holds only the business log:
    //     `[first_user, summary]`.
    //   * `outbound_messages` reattaches the system message that
    //     was passed in at session construction.
    let log = session.messages().await;
    let result = session.outbound_messages().await;

    assert!(
        matches!(
            log.as_slice(),
            [
                Message { role: Role::User, content: Content::Text(first), .. },
                Message { role: Role::User, content: Content::Text(summary), .. },
            ] if first == "first request" && summary.contains("[CONTEXT SUMMARY")
        ),
        "compaction should leave business log as first user + summary, got {log:?}"
    );
    assert!(
        matches!(
            result.as_slice(),
            [
                Message { role: Role::System, .. },
                Message { role: Role::User, content: Content::Text(first), .. },
                Message { role: Role::User, content: Content::Text(summary), .. },
            ] if first == "first request" && summary.contains("[CONTEXT SUMMARY")
        ),
        "outbound view should reattach system + first user + summary, got {result:?}"
    );
}

#[tokio::test]
async fn session_turn_owns_and_updates_message_history() {
    let agent = AIAgent::new(
        ScriptedProvider::new(vec![Completion {
            message: assistant_text("first answer"),
            usage: Usage {
                input_tokens: 1_000,
                output_tokens: 20,
                cached_input_tokens: 0,
            },
            finish_reason: FinishReason::Stop,
        }]),
        echo_config_with_compression(),
    );
    // The system message lives in its own field on the session,
    // not at the head of `messages`. `outbound_messages()` reattaches
    // it for the loop.
    let session = AgentSession::new(
        "test",
        std::env::current_dir().unwrap_or_default(),
        Some(system_message("system")),
    );

    let events = Arc::new(Mutex::new(Vec::new()));
    let result = agent
        .run_session_turn("first request", &session, CancellationToken::new(), {
            let events = Arc::clone(&events);
            move |event| events.lock().unwrap().push(event)
        })
        .await
        .expect("session turn should succeed");

    // `messages()` returns the business log only — the system
    // message is in its own field. `outbound_messages()` is what
    // the loop saw, including the system message.
    let history = session.messages().await;
    let outbound = session.outbound_messages().await;
    assert_eq!(outbound.len(), result.messages.len());
    assert_eq!(
        messages_to_text(&outbound),
        messages_to_text(&result.messages),
        "outbound view should match the run result"
    );
    assert!(matches!(
        history.as_slice(),
        [
            Message { role: Role::User, content: Content::Text(user), .. },
            Message { role: Role::Assistant, content: Content::Text(answer), .. },
        ] if user == "first request" && answer == "first answer"
    ));
    assert!(
        events.lock().unwrap().iter().any(|event| matches!(
            event,
            perry_hermes_agent::LoopEvent::ContextUsageUpdated { used_tokens: 1_000 }
        )),
        "session turn should publish provider-reported context usage"
    );
}

fn messages_to_text(messages: &[Message]) -> Vec<(&Role, String)> {
    messages
        .iter()
        .map(|message| (&message.role, message.content.as_text()))
        .collect()
}

#[tokio::test]
async fn session_compact_rewrites_session_messages() {
    let agent = AIAgent::new(
        ScriptedProvider::new(vec![Completion {
            message: assistant_text("condensed"),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 33,
                cached_input_tokens: 0,
            },
            finish_reason: FinishReason::Stop,
        }]),
        echo_config_with_compression(),
    );
    // System message is supplied via the dedicated field; the
    // business log contains only user/assistant turns.
    let session = AgentSession::new(
        "test",
        std::env::current_dir().unwrap_or_default(),
        Some(system_message("system")),
    );
    session
        .replace_messages(vec![
            user_message("first request"),
            assistant_text("first answer"),
            user_message("second request"),
            assistant_text("second answer"),
        ])
        .await;
    session.remember_context_usage_baseline(1_000).await;

    let event = agent
        .compact_session(&session, Some("second request"))
        .await
        .expect("session compact should succeed");

    match event {
        perry_hermes_agent::LoopEvent::CompressionCompleted {
            compacted_tokens, ..
        } => assert_eq!(compacted_tokens, Some(1_033)),
        other => panic!("expected CompressionCompleted event, got {other:?}"),
    }

    let log = session.messages().await;
    assert!(matches!(
        log.as_slice(),
        [
            Message { role: Role::User, content: Content::Text(first), .. },
            Message { role: Role::User, content: Content::Text(summary), .. },
        ] if first == "first request" && summary.contains("condensed")
    ));
    // The system message is preserved in the session's separate
    // field and reattaches at the head of `outbound_messages`.
    let outbound = session.outbound_messages().await;
    assert!(matches!(
        outbound.as_slice(),
        [
            Message { role: Role::System, .. },
            Message { role: Role::User, content: Content::Text(first), .. },
            Message { role: Role::User, content: Content::Text(summary), .. },
        ] if first == "first request" && summary.contains("condensed")
    ));
}

#[tokio::test]
async fn session_compact_reports_summary_failure() {
    let agent = AIAgent::new(
        ScriptedProvider::from_steps(vec![
            support::ScriptedStep::Error(ProviderError::Transport("summary provider down".into())),
            support::ScriptedStep::Error(ProviderError::Transport(
                "summary provider still down".into(),
            )),
        ]),
        echo_config_with_compression(),
    );

    let session = AgentSession::new(
        "test",
        std::env::current_dir().unwrap_or_default(),
        Some(system_message("system")),
    );
    session
        .replace_messages(vec![
            user_message("first request"),
            assistant_text("first answer"),
            user_message("second request"),
            assistant_text("second answer"),
            user_message(&"B".repeat(180_000)),
            assistant_text("large answer"),
            user_message("follow-up request"),
            assistant_text("follow-up answer"),
            user_message("latest request"),
        ])
        .await;

    let event = agent
        .compact_session(&session, Some("task-X"))
        .await
        .expect("manual compact should return a failure event, not a hard error");

    match event {
        perry_hermes_agent::LoopEvent::CompressionFailed { error, .. } => {
            assert!(
                error.contains("summary failed") || error.contains("summary provider down"),
                "unexpected error text: {error}"
            );
        }
        other => panic!("expected CompressionFailed event, got {other:?}"),
    }
}

#[tokio::test]
async fn session_compact_compresses_medium_history_instead_of_skipping() {
    let agent = AIAgent::new(
        ScriptedProvider::new(vec![Completion {
            message: assistant_text("condensed"),
            usage: Usage::default(),
            finish_reason: FinishReason::Stop,
        }]),
        echo_config_with_compression(),
    );

    let session = AgentSession::new(
        "test",
        std::env::current_dir().unwrap_or_default(),
        Some(system_message("system")),
    );
    session
        .replace_messages(vec![
            user_message("first request"),
            assistant_text("first answer"),
            user_message("second request"),
            assistant_text("second answer"),
            user_message("third request"),
            assistant_text("third answer"),
            user_message("follow-up request"),
            assistant_text("follow-up answer"),
            user_message("latest request"),
        ])
        .await;

    let event = agent
        .compact_session(&session, Some("latest request"))
        .await
        .expect("manual compact should succeed");

    match event {
        perry_hermes_agent::LoopEvent::CompressionCompleted { .. } => {}
        other => panic!("expected CompressionCompleted event, got {other:?}"),
    }

    let result = session.messages().await;
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
