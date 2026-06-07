use std::path::PathBuf;
use std::sync::Arc;

use hermes_core::message::Message;
use hermes_core::provider::Provider;
use hermes_core::tool::{ToolContext, ToolPermissions};
use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;

use crate::config::{HermesConfig, ResolvedProviderConfig};
use crate::prompting::{
    build_runtime_system_prompt, compose_base_system_prompt, inject_system_prompt,
    resolve_skills_dir,
};
use crate::provider_factory::build_provider;
use crate::session::SessionContext;
use crate::tool_catalog::build_registry;
use crate::{
    AgentLoop, AgentRunError, CompressorConfig, ContextCompressor, LoopConfig, LoopEvent, RunResult,
};

pub struct AIAgent {
    loop_: AgentLoop,
    base_system_prompt: Option<String>,
    provider_name: Option<String>,
}

impl AIAgent {
    pub fn from_config(config: HermesConfig) -> anyhow::Result<Self> {
        let selected_provider = config.resolve_provider()?;
        let provider_name = Some(selected_provider.name.clone());
        let base_system_prompt = compose_base_system_prompt(config.agent.system_prompt.as_deref());
        let provider = build_provider(&selected_provider)?;
        Ok(Self {
            loop_: build_loop(Arc::from(provider), &config, &selected_provider),
            base_system_prompt,
            provider_name,
        })
    }

    pub fn new(provider: impl Provider + 'static, config: HermesConfig) -> Self {
        let selected_provider = config.resolve_provider().ok();
        let provider_name = selected_provider
            .as_ref()
            .map(|provider| provider.name.clone());
        let base_system_prompt = compose_base_system_prompt(config.agent.system_prompt.as_deref());
        Self {
            loop_: build_loop_for_custom_provider(
                Arc::new(provider),
                &config,
                selected_provider.as_ref(),
            ),
            base_system_prompt,
            provider_name,
        }
    }

    pub async fn run_turn(
        &self,
        user_text: &str,
        session: &SessionContext,
        cancel: CancellationToken,
        on_event: impl FnMut(LoopEvent) + Send,
    ) -> Result<RunResult, AgentRunError> {
        self.run_messages(vec![Message::user(user_text)], session, cancel, on_event)
            .await
    }

    pub async fn run_messages(
        &self,
        messages: Vec<Message>,
        session: &SessionContext,
        cancel: CancellationToken,
        on_event: impl FnMut(LoopEvent) + Send,
    ) -> Result<RunResult, AgentRunError> {
        let ctx = ToolContext {
            session_id: session.session_id.clone(),
            working_dir: session.working_dir.clone(),
            permissions: ToolPermissions { subprocess: true },
        };
        let messages = inject_system_prompt(
            messages,
            self.base_system_prompt.as_deref().map(|base| {
                build_runtime_system_prompt(base, session, self.provider_name.as_deref())
            }),
        );
        self.loop_.run(messages, ctx, cancel, on_event).await
    }

    pub async fn run_compact(
        &self,
        messages: Vec<Message>,
        focus_topic: Option<&str>,
        session: &SessionContext,
    ) -> Result<(Vec<Message>, LoopEvent), AgentRunError> {
        let messages = inject_system_prompt(
            messages,
            self.base_system_prompt.as_deref().map(|base| {
                build_runtime_system_prompt(base, session, self.provider_name.as_deref())
            }),
        );
        self.loop_.compact_messages(messages, focus_topic).await
    }

    #[cfg(test)]
    fn has_context_engine(&self) -> bool {
        self.loop_.has_context_engine()
    }
}

fn build_loop(
    provider: Arc<dyn Provider>,
    config: &HermesConfig,
    selected_provider: &ResolvedProviderConfig,
) -> AgentLoop {
    build_loop_for_custom_provider(provider, config, Some(selected_provider))
}

fn build_loop_for_custom_provider(
    provider: Arc<dyn Provider>,
    config: &HermesConfig,
    selected_provider: Option<&ResolvedProviderConfig>,
) -> AgentLoop {
    let skills_dir = resolve_skills_dir().unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".perry_hermes")
            .join("skills")
    });
    let registry = Arc::new(build_registry(&config.agent.disabled_toolsets, &skills_dir));
    let context_engine = if config.agent.context_compression_enabled {
        let mut compressor_config = CompressorConfig::default();
        if let Some(threshold_percent) = config.agent.context_compression_threshold_percent {
            compressor_config.threshold_percent = threshold_percent;
        }
        let model_name = selected_provider
            .map(|provider| provider.model.clone())
            .unwrap_or_else(|| "custom".to_string());
        let context_window_size = selected_provider.map(|provider| provider.context_window_size);
        Some(Arc::new(TokioMutex::new(
            ContextCompressor::new(compressor_config, model_name, context_window_size)
                .with_summary_provider(Arc::clone(&provider)),
        ))
            as Arc<TokioMutex<dyn hermes_core::ContextEngine>>)
    } else {
        None
    };
    AgentLoop::from_provider(
        provider,
        registry,
        LoopConfig {
            max_iterations: config.agent.max_iterations.unwrap_or(10),
            system_prompt: None,
            context_engine,
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
    use hermes_core::message::Message;
    use hermes_core::provider::{
        CompletionDelta, CompletionStream, FinishReason, Provider, ToolCallDelta,
    };
    use hermes_core::registry::{InMemoryRegistry, ToolSchema};
    use hermes_core::tool::{Tool, ToolContext, ToolOutput};
    use hermes_core::{ProviderError, ToolError, Usage};
    use serde_json::{json, Value};
    use tokio_util::sync::CancellationToken;

    use crate::config::{ModelConfig, ProviderConfig, ProviderKind};

    fn echo_config() -> HermesConfig {
        HermesConfig {
            providers: vec![provider_config(
                "local",
                ProviderKind::Echo,
                "echo",
                128_000,
            )],
            agent: crate::config::AgentConfig {
                default_provider: "local".into(),
                default_model: "echo".into(),
                ..Default::default()
            },
        }
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

    fn echo_config_with_compression() -> HermesConfig {
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

    #[test]
    fn from_config_wires_context_engine_when_enabled() {
        let agent = AIAgent::from_config(echo_config_with_compression())
            .expect("echo should build with compression enabled");
        assert!(
            agent.has_context_engine(),
            "compression-enabled config should wire a context engine"
        );
    }

    #[test]
    fn from_config_errors_on_missing_model() {
        let config = HermesConfig {
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
        let config = HermesConfig {
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
        let config = HermesConfig {
            providers: vec![ProviderConfig {
                name: "openai-main".into(),
                kind: ProviderKind::Openai,
                api_key_env: Some("HERMES_TEST_DEFINITELY_NOT_SET_98765".into()),
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
        };
        let err = AIAgent::from_config(config)
            .err()
            .expect("expected from_config to fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("HERMES_TEST_DEFINITELY_NOT_SET_98765"),
            "error should name the missing env var: {msg}"
        );
    }

    #[test]
    fn from_config_errors_on_missing_anthropic_model() {
        let config = HermesConfig {
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
        };

        let err = AIAgent::from_config(config)
            .err()
            .expect("expected failure");
        let msg = format!("{err:#}");
        assert!(msg.contains("model"));
    }

    #[test]
    fn new_with_custom_provider_and_default_config() {
        use hermes_providers::EchoProvider;
        let agent = AIAgent::new(EchoProvider::new(), HermesConfig::default());
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
                        arguments_delta: Some("{}".into()),
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
    async fn session_context_is_plumbed_into_tool_context() {
        let captured: Arc<Mutex<Option<ToolContext>>> = Arc::new(Mutex::new(None));

        let registry = InMemoryRegistry::new().register(Arc::new(CaptureTool {
            captured: Arc::clone(&captured),
        }));

        let provider = OneToolCallProvider {
            calls: Arc::new(Mutex::new(0)),
        };
        let loop_ = AgentLoop::new(
            provider,
            Arc::new(registry),
            LoopConfig {
                max_iterations: 3,
                ..Default::default()
            },
        );
        let agent = AIAgent {
            loop_,
            base_system_prompt: None,
            provider_name: None,
        };

        let session = SessionContext {
            working_dir: std::path::PathBuf::from("/tmp/hermes-test-cwd"),
            session_id: "session-xyz".into(),
        };

        let cancel = CancellationToken::new();
        agent
            .run_turn("hi", &session, cancel, |_| {})
            .await
            .expect("run should succeed");

        let ctx = captured.lock().unwrap().clone().expect("tool was called");
        assert_eq!(
            ctx.working_dir,
            std::path::PathBuf::from("/tmp/hermes-test-cwd")
        );
        assert_eq!(ctx.session_id, "session-xyz");
    }
}
