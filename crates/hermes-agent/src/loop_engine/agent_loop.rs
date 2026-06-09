//! The agent loop — calls the LLM, reacts to `finish_reason`, dispatches
//! tools, returns a `RunResult`.
//!
//! Sub-modules:
//! - `metrics` — provider usage helpers + `validate_args`
//! - `run` — the state machine (`run`, `drive_turn`, `handle_finish_reason`, `dispatch_tool_calls`)

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;

use perry_hermes_core::compaction_strategy::{
    CompactionStrategy, CompressionSkipReason, CompressionTrigger,
};
use perry_hermes_core::error::{LoopError, ProviderError, ToolError};
use perry_hermes_core::message::{Message, ToolCall};
use perry_hermes_core::provider::{Provider, ToolCallDelta};
use perry_hermes_core::registry::InMemoryRegistry;
use perry_hermes_core::tool::{ToolContext, ToolOutput};

use crate::config::{PerryHermesConfig, ResolvedProviderConfig};
use crate::prompting::{build_system_message, resolve_skills_dir};
use crate::provider_factory::build_provider;
use crate::session::AgentSession;
use crate::tool_catalog::build_registry;
use crate::{CompactorConfig, SummaryCompactor, loop_engine};

pub struct AgentLoop {
    pub(crate) provider: Arc<dyn Provider>,
    pub(crate) registry: Arc<InMemoryRegistry>,
    pub(crate) config: LoopConfig,
}

#[derive(Clone)]
pub struct LoopConfig {
    pub max_iterations: u32,
    pub max_duration: Duration,
    pub system_prompt: Option<String>,
    /// Optional context compaction strategy. None = no compaction.
    pub compaction_strategy: Option<Arc<TokioMutex<dyn CompactionStrategy>>>,
    /// Model context window and compression threshold used with real
    /// provider usage.
    pub context_window: Option<ContextWindow>,
    /// Focus topic for manual `/compact [focus]`.
    pub focus_topic: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct ContextWindow {
    pub max_tokens: u64,
    pub compression_threshold_ratio: f64,
}

impl ContextWindow {
    pub fn threshold_tokens(self) -> u64 {
        (self.max_tokens as f64 * self.compression_threshold_ratio) as u64
    }

    pub fn should_compress(self, used_tokens: u64) -> bool {
        used_tokens >= self.threshold_tokens()
    }
}

impl std::fmt::Debug for LoopConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopConfig")
            .field("max_iterations", &self.max_iterations)
            .field("max_duration", &self.max_duration)
            .field("system_prompt", &self.system_prompt)
            .field("compaction_strategy", &"<dyn CompactionStrategy>")
            .field("context_window", &self.context_window)
            .field("focus_topic", &self.focus_topic)
            .finish()
    }
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: 90,
            max_duration: Duration::from_secs(60 * 10),
            system_prompt: None,
            compaction_strategy: None,
            context_window: None,
            focus_topic: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LoopMetrics {
    pub iterations: u32,
    pub tool_calls: u32,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub duration: Duration,
    pub compressions: u32,
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub final_message: Message,
    pub messages: Vec<Message>,
    pub metrics: LoopMetrics,
}

#[derive(Debug, Clone)]
pub struct FailedTurn {
    pub messages: Vec<Message>,
    pub error: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentRunError {
    #[error(transparent)]
    Loop(#[from] LoopError),
    #[error("provider error with partial response: {source}")]
    FailedTurn {
        failed_turn: FailedTurn,
        #[source]
        source: ProviderError,
    },
}

#[derive(Debug, Clone)]
pub enum LoopEvent {
    Thinking,
    ContentDelta(String),
    ReasoningDelta(String),
    ToolCallPartial(ToolCallDelta),
    AssistantMessage(Message),
    ToolCallStarted {
        call: ToolCall,
        iteration: u32,
    },
    ToolCallFinished {
        call: ToolCall,
        result: Result<ToolOutput, ToolError>,
    },
    LengthLimit,
    IterationsExhausted,
    Cancelled,
    ContextUsageUpdated {
        used_tokens: u64,
    },
    CompressionCompleted {
        trigger: CompressionTrigger,
        /// Provider-reported prompt context tokens that caused automatic
        /// compression. `None` for manual `/compact`.
        context_tokens: Option<u64>,
        /// Best known post-compaction context usage, derived from the first
        /// prompt-context baseline plus the summary call's output tokens.
        compacted_tokens: Option<u64>,
        duration: Duration,
    },
    CompressionSkipped {
        reason: CompressionSkipReason,
    },
    CompressionFailed {
        trigger: CompressionTrigger,
        error: String,
    },
}

impl AgentLoop {
    // ── Low-level constructor ──────────────────────────────────────────

    /// Low-level constructor: takes pre-built provider, registry, and
    /// loop config. Prefer [`AgentLoop::from_config`] or
    /// [`AgentLoop::new`] for production use; this is mainly for tests
    /// that need precise control over the components.
    pub fn from_parts(
        provider: Arc<dyn Provider>,
        registry: Arc<InMemoryRegistry>,
        config: LoopConfig,
    ) -> Self {
        Self {
            provider,
            registry,
            config,
        }
    }

    // ── High-level constructors ────────────────────────────────────────

    /// Build an `AgentLoop` from a [`PerryHermesConfig`], resolving the
    /// provider from the config.
    ///
    /// This is the primary production constructor.
    pub fn from_config(config: PerryHermesConfig) -> anyhow::Result<Self> {
        let selected_provider = config.resolve_provider()?;
        let provider = build_provider(&selected_provider)?;
        Ok(build_loop_for_custom_provider(
            Arc::from(provider),
            &config,
            Some(&selected_provider),
        ))
    }

    /// Build an `AgentLoop` with a custom provider, using the given
    /// config for all other settings (tool registry, compaction, etc.).
    ///
    /// Public constructor: takes PerryHermesConfig by value so callers can
    /// move their config in. This is the public API — changing the
    /// signature would break every CLI and test caller. The clippy
    /// suggestion to take &PerryHermesConfig is rejected intentionally.
    pub fn new(provider: impl Provider + 'static, config: PerryHermesConfig) -> Self {
        let selected_provider = config.resolve_provider().ok();
        build_loop_for_custom_provider(Arc::new(provider), &config, selected_provider.as_ref())
    }

    // ── Session management ─────────────────────────────────────────────

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
    /// Includes the hardcoded base prompt, skills index, AGENTS.md
    /// content, and working directory hint.
    pub fn system_message_for(&self, working_dir: &std::path::Path) -> Option<Message> {
        build_system_message(working_dir)
    }

    /// Load a previously persisted session from a JSON snapshot,
    /// rebuilding the system message for the current working directory.
    pub async fn load_json_session(
        &self,
        path: impl Into<PathBuf>,
    ) -> std::io::Result<AgentSession> {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let system_message = self.system_message_for(&working_dir);
        AgentSession::load_json_file_with_system_message(path, Some(working_dir), system_message)
            .await
    }

    // ── Running a turn ─────────────────────────────────────────────────

    /// Run a single conversational turn: append the user's text to the
    /// session, then drive the agent loop to completion.
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
            permissions: perry_hermes_core::tool::ToolPermissions { subprocess: true },
        };
        self.run(messages, ctx, session, cancel, on_event).await
    }

    // ── Compaction ─────────────────────────────────────────────────────

    /// Compact a session's message history, replacing the business log
    /// with the compacted result.
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

    async fn compact_messages_for_session(
        &self,
        messages: Vec<Message>,
        focus_topic: Option<&str>,
        session: &AgentSession,
    ) -> Result<(Vec<Message>, LoopEvent), AgentRunError> {
        self.compact_messages(messages, focus_topic, session).await
    }

    // ── Core loop engine methods ───────────────────────────────────────

    /// Run the compaction strategy against `messages`, returning the
    /// compacted message list and a `LoopEvent` describing the outcome.
    pub async fn compact_messages(
        &self,
        mut messages: Vec<Message>,
        focus_topic: Option<&str>,
        session: &AgentSession,
    ) -> Result<(Vec<Message>, LoopEvent), AgentRunError> {
        let Some(engine) = &self.config.compaction_strategy else {
            return Ok((
                messages,
                LoopEvent::CompressionSkipped {
                    reason: CompressionSkipReason::Disabled,
                },
            ));
        };
        let outcome = crate::compaction::try_compact(
            engine,
            &mut messages,
            focus_topic,
            self.config.focus_topic.as_deref(),
        )
        .await
        .unwrap_or_else(|| crate::compaction::CompactOutcome::Failed {
            error: "compression failed".into(),
        });
        let event = match outcome {
            crate::compaction::CompactOutcome::Compressed {
                duration,
                summary_output_tokens,
            } => {
                let compacted_tokens = session
                    .compacted_context_tokens(summary_output_tokens)
                    .await;
                LoopEvent::CompressionCompleted {
                    trigger: CompressionTrigger::Manual,
                    context_tokens: None,
                    compacted_tokens,
                    duration,
                }
            }
            crate::compaction::CompactOutcome::Skipped(reason) => {
                LoopEvent::CompressionSkipped { reason }
            }
            crate::compaction::CompactOutcome::Failed { error } => LoopEvent::CompressionFailed {
                trigger: CompressionTrigger::Manual,
                error,
            },
        };
        Ok((messages, event))
    }

    /// Run the agent loop: stream completions from the provider, dispatch
    /// tool calls, handle compaction, and return a `RunResult`.
    pub async fn run(
        &self,
        initial_messages: Vec<Message>,
        ctx: ToolContext,
        session: &AgentSession,
        cancel: CancellationToken,
        on_event: impl FnMut(LoopEvent) + Send,
    ) -> Result<RunResult, AgentRunError> {
        loop_engine::run::run(self, initial_messages, ctx, session, cancel, on_event).await
    }
}

// ── Private helpers ───────────────────────────────────────────────────

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
        )) as Arc<TokioMutex<dyn CompactionStrategy>>)
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
    AgentLoop::from_parts(
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
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use futures::stream;
    use perry_hermes_core::message::Message;
    use perry_hermes_core::provider::{
        CompletionDelta, CompletionStream, FinishReason, ToolCallDelta,
    };
    use perry_hermes_core::registry::ToolSchema;
    use perry_hermes_core::tool::{Tool, ToolOutput};
    use perry_hermes_core::{ProviderError, ToolError, Usage};
    use serde_json::{Value, json};

    use crate::config::{ModelConfig, ProviderConfig, ProviderKind};

    fn echo_config() -> PerryHermesConfig {
        PerryHermesConfig::for_test_echo()
    }

    fn echo_config_with_compression() -> PerryHermesConfig {
        let mut config = echo_config();
        config.agent.context_compression_enabled = true;
        config
    }

    #[test]
    fn from_config_succeeds_for_echo_provider() {
        let agent =
            AgentLoop::from_config(echo_config()).expect("echo should build with no env vars");
        drop(agent);
    }

    #[tokio::test]
    async fn from_config_wires_compaction_strategy_when_enabled() {
        let agent = AgentLoop::from_config(echo_config_with_compression())
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
        let err = AgentLoop::from_config(config)
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
        let err = AgentLoop::from_config(config)
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
        let err = AgentLoop::from_config(config)
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

        let err = AgentLoop::from_config(config)
            .err()
            .expect("expected failure");
        let msg = format!("{err:#}");
        assert!(msg.contains("model"));
    }

    #[test]
    fn new_with_custom_provider_and_default_config() {
        use perry_hermes_providers::EchoProvider;
        let agent = AgentLoop::new(EchoProvider::new(), PerryHermesConfig::default());
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
        let _guard = crate::test_env::lock().await;
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

        let agent = AgentLoop::new(perry_hermes_providers::EchoProvider::new(), echo_config());
        let session = agent
            .load_json_session(path)
            .await
            .expect("session should load");
        let current_cwd = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        let session_cwd = std::fs::canonicalize(session.working_dir.as_ref()).unwrap();

        assert_eq!(session_cwd, current_cwd);

        let outbound = session.outbound_messages().await;
        let system_text = outbound[0].content.as_text();
        assert!(system_text.contains("Perry Hermes"));
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
        let agent = AgentLoop::from_parts(
            Arc::new(provider),
            Arc::new(registry),
            LoopConfig {
                max_iterations: 3,
                ..Default::default()
            },
        );

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
