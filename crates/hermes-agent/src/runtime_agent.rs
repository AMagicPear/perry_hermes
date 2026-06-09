use std::path::PathBuf;
use std::sync::Arc;

use perry_hermes_core::message::Message;
use perry_hermes_core::provider::Provider;
use perry_hermes_core::tool::{ToolContext, ToolPermissions};
use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;

use crate::config::{PerryHermesConfig, ResolvedProviderConfig};
use crate::prompting::{build_system_message, resolve_skills_dir};
use crate::provider_factory::build_provider;
use crate::session::AgentSession;
use crate::tool_catalog::build_registry;
use crate::{
    AgentLoop, AgentRunError, CompactorConfig, ContextWindow, LoopConfig, LoopEvent, RunResult,
    SummaryCompactor,
};

pub struct AIAgent {
    agent_loop: AgentLoop,
    /// User-configured system prompt. Skills, AGENTS.md, and working-dir
    /// hints are folded in once when a new session is created.
    system_prompt: Option<String>,
}

impl AIAgent {
    // Public constructor: takes PerryHermesConfig by value so callers can
    // move their config in. This is the public API — changing the
    // signature would break every CLI and test caller. The clippy
    // suggestion to take &PerryHermesConfig is rejected intentionally.
    #[allow(clippy::needless_pass_by_value)]
    pub fn from_config(config: PerryHermesConfig) -> anyhow::Result<Self> {
        let selected_provider = config.resolve_provider()?;
        let provider = build_provider(&selected_provider)?;
        Ok(Self {
            agent_loop: build_loop(Arc::from(provider), &config, &selected_provider),
            system_prompt: config.agent.system_prompt.clone(),
        })
    }

    // Public constructor: takes PerryHermesConfig by value so callers can
    // move their config in (public API; see from_config comment).
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(provider: impl Provider + 'static, config: PerryHermesConfig) -> Self {
        let selected_provider = config.resolve_provider().ok();
        Self {
            agent_loop: build_loop_for_custom_provider(
                Arc::new(provider),
                &config,
                selected_provider.as_ref(),
            ),
            system_prompt: config.agent.system_prompt.clone(),
        }
    }

    /// Construct a new `AgentSession`, computing the immutable system
    /// message for this session. The system message is stored at
    /// `AgentSession::system_message` and never recomposed on subsequent
    /// turns.
    pub fn new_session(
        &self,
        session_id: impl Into<String>,
        working_dir: impl Into<PathBuf>,
    ) -> AgentSession {
        let working_dir = working_dir.into();
        let system_message = self.system_message_for(&working_dir);
        AgentSession::new(session_id, working_dir, system_message)
    }

    /// Build the system message for a session at `working_dir`.
    /// Includes the user's system prompt, AGENTS.md content, working
    /// directory hint, and skills index.
    pub fn system_message_for(&self, working_dir: &std::path::Path) -> Option<Message> {
        build_system_message(self.system_prompt.as_deref(), working_dir)
    }

    pub async fn load_json_session(
        &self,
        path: impl Into<PathBuf>,
    ) -> std::io::Result<AgentSession> {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let system_message = self.system_message_for(&working_dir);
        AgentSession::load_json_file_with_system_message(path, Some(working_dir), system_message)
            .await
    }

    pub async fn run_session_turn(
        &self,
        user_text: &str,
        session: &AgentSession,
        cancel: CancellationToken,
        on_event: impl FnMut(LoopEvent) + Send,
    ) -> Result<RunResult, AgentRunError> {
        session.append_message(Message::user(user_text)).await;
        self.run_current_session(session, cancel, on_event).await
    }

    pub(crate) async fn run_current_session(
        &self,
        session: &AgentSession,
        cancel: CancellationToken,
        on_event: impl FnMut(LoopEvent) + Send,
    ) -> Result<RunResult, AgentRunError> {
        // The loop sees the full outbound stream: system message
        // (if any) followed by the business message log.
        let messages = session.outbound_messages().await;
        let result = self
            .run_messages_for_session(messages, session, cancel, on_event)
            .await;
        match &result {
            Ok(run_result) => {
                // Strip the system message back out — it lives in
                // its own field on the session. The remaining log
                // is the new business message history.
                let business = strip_system_message(&run_result.messages);
                session.replace_messages(business).await;
            }
            Err(AgentRunError::FailedTurn { failed_turn, .. }) => {
                let business = strip_system_message(&failed_turn.messages);
                session.replace_messages(business).await;
            }
            Err(_) => {}
        }
        result
    }

    async fn run_messages_for_session(
        &self,
        messages: Vec<Message>,
        session: &AgentSession,
        cancel: CancellationToken,
        on_event: impl FnMut(LoopEvent) + Send,
    ) -> Result<RunResult, AgentRunError> {
        let ctx = ToolContext {
            session_id: session.session_id.to_string(),
            working_dir: session.working_dir.as_ref().clone(),
            permissions: ToolPermissions { subprocess: true },
        };
        self.agent_loop
            .run(messages, ctx, session, cancel, on_event)
            .await
    }

    async fn compact_messages_for_session(
        &self,
        messages: Vec<Message>,
        focus_topic: Option<&str>,
        session: &AgentSession,
    ) -> Result<(Vec<Message>, LoopEvent), AgentRunError> {
        self.agent_loop
            .compact_messages(messages, focus_topic, session)
            .await
    }

    pub async fn compact_session(
        &self,
        session: &AgentSession,
        focus_topic: Option<&str>,
    ) -> Result<LoopEvent, AgentRunError> {
        // The compactor sees the full outbound stream so it can
        // preserve the system message and first user message as
        // anchors. After compaction, the system message is
        // reattached by the session and only the business portion
        // is written back to the log.
        let messages = session.outbound_messages().await;
        let (messages, event) = self
            .compact_messages_for_session(messages, focus_topic, session)
            .await?;
        if matches!(event, LoopEvent::CompressionCompleted { .. }) {
            let business = strip_system_message(&messages);
            session.replace_messages(business).await;
        }
        Ok(event)
    }
}

#[cfg(test)]
impl AIAgent {
    fn for_test(agent_loop: AgentLoop) -> Self {
        Self {
            agent_loop,
            system_prompt: None,
        }
    }
}

/// Drop any leading system message(s) from a list of messages.
/// Used by the session to translate between the loop's outbound
/// representation (which always includes the system message) and
/// the session's storage representation (where the system message
/// lives in its own field).
fn strip_system_message(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .filter(|m| m.role != perry_hermes_core::message::Role::System)
        .cloned()
        .collect()
}

fn build_loop(
    provider: Arc<dyn Provider>,
    config: &PerryHermesConfig,
    selected_provider: &ResolvedProviderConfig,
) -> AgentLoop {
    build_loop_for_custom_provider(provider, config, Some(selected_provider))
}

fn build_loop_for_custom_provider(
    provider: Arc<dyn Provider>,
    config: &PerryHermesConfig,
    selected_provider: Option<&ResolvedProviderConfig>,
) -> AgentLoop {
    let skills_dir = resolve_skills_dir().unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".perry_hermes")
            .join("skills")
    });
    let registry = Arc::new(build_registry(&config.agent.disabled_toolsets, &skills_dir));
    let compaction_strategy = if config.agent.context_compression_enabled {
        let compactor_config = CompactorConfig::default();
        Some(Arc::new(TokioMutex::new(
            SummaryCompactor::new(compactor_config).with_summary_provider(Arc::clone(&provider)),
        ))
            as Arc<TokioMutex<dyn perry_hermes_core::CompactionStrategy>>)
    } else {
        None
    };
    let context_window = selected_provider.map(|provider| ContextWindow {
        max_tokens: provider.context_window_size,
        compression_threshold_ratio: config
            .agent
            .context_compression_threshold_percent
            .unwrap_or(0.50),
    });
    AgentLoop::from_provider(
        provider,
        registry,
        LoopConfig {
            max_iterations: config.agent.max_iterations.unwrap_or(10),
            system_prompt: None,
            compaction_strategy,
            context_window,
            ..Default::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use futures::stream;
    use perry_hermes_core::message::Message;
    use perry_hermes_core::provider::{
        CompletionDelta, CompletionStream, FinishReason, Provider, ToolCallDelta,
    };
    use perry_hermes_core::registry::{InMemoryRegistry, ToolSchema};
    use perry_hermes_core::tool::{Tool, ToolContext, ToolOutput};
    use perry_hermes_core::{ProviderError, ToolError, Usage};
    use serde_json::{Value, json};
    use tokio_util::sync::CancellationToken;

    use crate::config::test_helpers::*;
    use crate::config::{ModelConfig, ProviderConfig, ProviderKind};
    fn echo_config() -> PerryHermesConfig {
        PerryHermesConfig::for_test_echo()
    }

    fn provider_config(
        name: &str,
        kind: ProviderKind,
        model: &str,
        context_window_size: u64,
    ) -> ProviderConfig {
        ProviderConfig {
            name: name.into(),
            kind,
            api_key_env: None,
            models: vec![ModelConfig {
                name: model.into(),
                context_window_size,
            }],
            base_url: None,
            api_key_header: None,
            thinking: None,
        }
    }

    fn echo_config_with_compression() -> PerryHermesConfig {
        let mut config = echo_config();
        config.agent.context_compression_enabled = true;
        config
    }

    #[test]
    fn from_config_succeeds_for_echo_provider() {
        let agent =
            AIAgent::from_config(echo_config()).expect("echo should build with no env vars");
        drop(agent);
    }

    #[tokio::test]
    async fn from_config_wires_compaction_strategy_when_enabled() {
        let agent = AIAgent::from_config(echo_config_with_compression())
            .expect("echo should build with compression enabled");
        let session = AgentSession::new("s", PathBuf::from("/tmp"), None);
        session.append_message(Message::user("first")).await;

        let event = agent
            .compact_session(&session, None)
            .await
            .expect("manual compaction should be callable");
        assert!(matches!(
            event,
            LoopEvent::CompressionSkipped {
                reason:
                    perry_hermes_core::compaction_strategy::CompressionSkipReason::NothingToCompress
            }
        ));
    }

    #[test]
    fn from_config_errors_on_missing_model() {
        let config = PerryHermesConfig {
            providers: vec![ProviderConfig {
                name: "openai-main".into(),
                kind: ProviderKind::Openai,
                api_key_env: None,
                models: vec![ModelConfig {
                    name: "gpt-4o-mini".into(),
                    context_window_size: 128_000,
                }],
                base_url: Some("https://api.openai.com/v1".into()),
                api_key_header: None,
                thinking: None,
            }],
            agent: crate::config::AgentConfig {
                default_provider: "openai-main".into(),
                default_model: "missing-model".into(),
                ..Default::default()
            },
            gateway: Default::default(),
        };
        let err = AIAgent::from_config(config)
            .err()
            .expect("expected from_config to fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("model"),
            "error should name the missing field: {msg}"
        );
    }

    #[test]
    fn from_config_errors_on_missing_base_url() {
        let config = PerryHermesConfig {
            providers: vec![ProviderConfig {
                name: "openai-main".into(),
                kind: ProviderKind::Openai,
                api_key_env: None,
                models: vec![ModelConfig {
                    name: "gpt-4o-mini".into(),
                    context_window_size: 128_000,
                }],
                base_url: None,
                api_key_header: None,
                thinking: None,
            }],
            agent: crate::config::AgentConfig {
                default_provider: "openai-main".into(),
                default_model: "gpt-4o-mini".into(),
                ..Default::default()
            },
            gateway: Default::default(),
        };
        let err = AIAgent::from_config(config)
            .err()
            .expect("expected from_config to fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("base_url"),
            "error should name the missing field: {msg}"
        );
    }

    #[test]
    fn from_config_errors_on_missing_api_key_env() {
        let config = PerryHermesConfig {
            providers: vec![ProviderConfig {
                name: "openai-main".into(),
                kind: ProviderKind::Openai,
                api_key_env: Some("PERRY_HERMES_TEST_DEFINITELY_NOT_SET_98765".into()),
                models: vec![ModelConfig {
                    name: "gpt-4o-mini".into(),
                    context_window_size: 128_000,
                }],
                base_url: Some("https://api.openai.com/v1".into()),
                api_key_header: None,
                thinking: None,
            }],
            agent: crate::config::AgentConfig {
                default_provider: "openai-main".into(),
                default_model: "gpt-4o-mini".into(),
                ..Default::default()
            },
            gateway: Default::default(),
        };
        let err = AIAgent::from_config(config)
            .err()
            .expect("expected from_config to fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("PERRY_HERMES_TEST_DEFINITELY_NOT_SET_98765"),
            "error should name the missing env var: {msg}"
        );
    }

    #[test]
    fn from_config_errors_on_missing_anthropic_model() {
        let config = PerryHermesConfig {
            providers: vec![ProviderConfig {
                name: "anthropic-main".into(),
                kind: ProviderKind::Anthropic,
                api_key_env: None,
                models: vec![ModelConfig {
                    name: "claude-sonnet".into(),
                    context_window_size: 200_000,
                }],
                base_url: Some("https://api.anthropic.com/v1".into()),
                api_key_header: None,
                thinking: None,
            }],
            agent: crate::config::AgentConfig {
                default_provider: "anthropic-main".into(),
                default_model: "missing-model".into(),
                ..Default::default()
            },
            gateway: Default::default(),
        };

        let err = AIAgent::from_config(config)
            .err()
            .expect("expected failure");
        let msg = format!("{err:#}");
        assert!(msg.contains("model"));
    }

    #[test]
    fn new_with_custom_provider_and_default_config() {
        use perry_hermes_providers::EchoProvider;
        let agent = AIAgent::new(EchoProvider::new(), PerryHermesConfig::default());
        drop(agent);
    }

    struct OneToolCallProvider {
        calls: Arc<Mutex<u32>>,
    }

    #[async_trait]
    impl Provider for OneToolCallProvider {
        async fn stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _cancel: CancellationToken,
        ) -> Result<CompletionStream, ProviderError> {
            let n = {
                let mut g = self.calls.lock().unwrap();
                *g += 1;
                *g
            };
            let deltas = if n == 1 {
                vec![CompletionDelta {
                    content_delta: None,
                    reasoning_delta: None,
                    tool_call_delta: Some(ToolCallDelta {
                        index: 0,
                        id: Some("call_1".into()),
                        name: Some("capture".into()),
                        arguments_fragment: Some("{}".into()),
                    }),
                    usage: Some(Usage::default()),
                    finish_reason: Some(FinishReason::ToolUse),
                }]
            } else {
                vec![CompletionDelta {
                    content_delta: Some("done".into()),
                    reasoning_delta: None,
                    tool_call_delta: None,
                    usage: Some(Usage::default()),
                    finish_reason: Some(FinishReason::Stop),
                }]
            };
            Ok(Box::pin(stream::iter(deltas.into_iter().map(Ok))))
        }
    }

    struct CaptureTool {
        captured: Arc<Mutex<Option<ToolContext>>>,
    }

    #[async_trait]
    impl Tool for CaptureTool {
        fn name(&self) -> &str {
            "capture"
        }
        fn description(&self) -> &str {
            "test tool that captures ToolContext"
        }
        fn parameters_schema(&self) -> Value {
            json!({"type": "object", "properties": {}})
        }
        fn toolset(&self) -> &'static str {
            "core"
        }
        async fn execute(
            &self,
            _args: Value,
            ctx: ToolContext,
            _cancel: CancellationToken,
        ) -> Result<ToolOutput, ToolError> {
            *self.captured.lock().unwrap() = Some(ctx);
            Ok(ToolOutput {
                content: "ok".into(),
            })
        }
    }

    #[tokio::test]
    async fn load_json_session_uses_current_cwd_and_rebuilds_system_message() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sessions").join("session-1.json");
        let saved = AgentSession::new(
            "session-1",
            PathBuf::from("/tmp/original-project"),
            Some(Message::system(
                "OLD SYSTEM\n\nCurrent working directory: /tmp/original-project",
            )),
        )
        .with_json_file_store(path.clone());
        saved.append_message(Message::user("saved hello")).await;
        saved.remember_context_usage_baseline(123).await;

        let mut config = echo_config();
        config.agent.system_prompt = Some("NEW SYSTEM".into());
        let agent = AIAgent::new(perry_hermes_providers::EchoProvider::new(), config);
        let session = agent
            .load_json_session(path)
            .await
            .expect("session should load");
        let current_cwd = std::env::current_dir().unwrap();

        assert_eq!(session.working_dir.as_ref(), &current_cwd);

        let outbound = session.outbound_messages().await;
        let system_text = outbound[0].content.as_text();
        assert!(system_text.contains("NEW SYSTEM"));
        assert!(system_text.contains(&format!(
            "Current working directory: {}",
            current_cwd.display()
        )));
        assert!(!system_text.contains("OLD SYSTEM"));
        assert!(!system_text.contains("/tmp/original-project"));
        assert_eq!(outbound[1].content.as_text(), "saved hello");
        assert_eq!(session.compacted_context_tokens(7).await, Some(130));
    }

    #[tokio::test]
    async fn session_context_is_plumbed_into_tool_context() {
        let captured: Arc<Mutex<Option<ToolContext>>> = Arc::new(Mutex::new(None));

        let registry = InMemoryRegistry::new().register(Arc::new(CaptureTool {
            captured: Arc::clone(&captured),
        }));

        let provider = OneToolCallProvider {
            calls: Arc::new(Mutex::new(0)),
        };
        let agent_loop = AgentLoop::new(
            provider,
            Arc::new(registry),
            LoopConfig {
                max_iterations: 3,
                ..Default::default()
            },
        );
        let agent = AIAgent::for_test(agent_loop);

        let session = AgentSession::new(
            "session-xyz",
            std::path::PathBuf::from("/tmp/perry-hermes-test-cwd"),
            None,
        );

        let cancel = CancellationToken::new();
        agent
            .run_session_turn("hi", &session, cancel, |_| {})
            .await
            .expect("run should succeed");

        let ctx = captured.lock().unwrap().clone().expect("tool was called");
        assert_eq!(
            ctx.working_dir,
            std::path::PathBuf::from("/tmp/perry-hermes-test-cwd")
        );
        assert_eq!(ctx.session_id, "session-xyz");
    }
}
