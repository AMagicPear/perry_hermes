use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::Utc;
use futures::stream;
use perry_hermes_agent::{
    AgentConfig, AgentLoop, ModelConfig, PerryHermesConfig, ProviderConfig, ProviderKind,
};
use perry_hermes_core::message::{Content, Message, Role};
use perry_hermes_core::provider::{CompletionDelta, CompletionStream, FinishReason, Provider};
use perry_hermes_core::registry::ToolSchema;
use perry_hermes_core::{Platform, ProviderError, Usage};
use perry_hermes_gateway::{
    ChatType, GatewayConfig, GatewayEvent, GatewayEventHandler, GatewayResponse, GatewayRunner,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct RecordingProviderState {
    calls: AtomicUsize,
    first_call_started: Notify,
    release_first_call: Notify,
}

#[derive(Clone)]
struct RecordingProvider {
    state: Arc<RecordingProviderState>,
}

#[async_trait]
impl Provider for RecordingProvider {
    async fn stream(
        &self,
        messages: &[Message],
        _tools: &[ToolSchema],
        _cancel: CancellationToken,
    ) -> Result<CompletionStream, ProviderError> {
        let call_index = self.state.calls.fetch_add(1, Ordering::SeqCst);
        if call_index == 0 {
            self.state.first_call_started.notify_waiters();
            self.state.release_first_call.notified().await;
        }

        let last_user = messages
            .iter()
            .rev()
            .find(|message| message.role == Role::User)
            .map(|message| message.content.as_text())
            .unwrap_or_default();
        let delta = CompletionDelta {
            content_delta: Some(format!("reply {call_index}: {last_user}")),
            reasoning_delta: None,
            tool_call_delta: None,
            usage: Some(Usage::default()),
            finish_reason: Some(FinishReason::Stop),
        };
        Ok(Box::pin(stream::once(async move { Ok(delta) })))
    }
}

struct TestHandler;

impl GatewayEventHandler for TestHandler {}

fn agent(provider: RecordingProvider) -> Arc<AgentLoop> {
    Arc::new(AgentLoop::new(
        provider,
        PerryHermesConfig {
            providers: vec![ProviderConfig {
                name: "local".into(),
                kind: ProviderKind::Echo,
                api_key_env: None,
                models: vec![ModelConfig {
                    name: "test".into(),
                    context_window_size: 128_000,
                }],
                base_url: None,
                api_key_header: None,
                thinking: None,
            }],
            agent: AgentConfig {
                default_provider: "local".into(),
                default_model: "test".into(),
                ..AgentConfig::default()
            },
            gateway: Default::default(),
        },
    ))
}

fn event(text: &str) -> GatewayEvent {
    GatewayEvent {
        platform: Platform::Telegram,
        chat_id: "chat-1".into(),
        chat_type: ChatType::Dm,
        user_id: "user-1".into(),
        user_name: None,
        thread_id: None,
        text: text.into(),
        message_id: None,
        timestamp: Utc::now(),
    }
}

#[tokio::test]
async fn concurrent_gateway_message_batched_by_active_turn_does_not_start_empty_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let provider_state = Arc::new(RecordingProviderState::default());
    let runner = Arc::new(GatewayRunner::new(
        agent(RecordingProvider {
            state: Arc::clone(&provider_state),
        }),
        GatewayConfig {
            sessions_dir: tmp.path().join("sessions"),
            working_dir: PathBuf::from("/tmp/project"),
            ..GatewayConfig::default()
        },
    ));

    let first_runner = Arc::clone(&runner);
    let first = tokio::spawn(async move {
        let mut handler = TestHandler;
        first_runner
            .handle_event(event("first"), &mut handler)
            .await
            .expect("first event should run")
    });

    provider_state.first_call_started.notified().await;

    let second_runner = Arc::clone(&runner);
    let second = tokio::spawn(async move {
        let mut handler = TestHandler;
        second_runner
            .handle_event(event("second"), &mut handler)
            .await
            .expect("second event should be consumed by first active turn")
    });

    while provider_state.calls.load(Ordering::SeqCst) < 1 {
        tokio::task::yield_now().await;
    }
    provider_state.release_first_call.notify_waiters();

    assert!(matches!(first.await.unwrap(), GatewayResponse::Ignored));
    assert!(matches!(second.await.unwrap(), GatewayResponse::Ignored));
    assert_eq!(
        provider_state.calls.load(Ordering::SeqCst),
        2,
        "second message should produce one follow-up provider call, not an extra empty turn"
    );

    let messages = runner
        .sessions()
        .get_session("telegram:dm:chat-1")
        .expect("session should exist")
        .messages()
        .await;
    assert!(
        messages
            .iter()
            .filter(|message| matches!(&message.content, Content::Text(text) if text.is_empty()))
            .count()
            == 0,
        "batched gateway messages must not persist a blank user message"
    );
}
