use std::path::PathBuf;
use std::sync::Arc;

use crate::{AgentLoop, LoopConfig, LoopEvent, RunResult};
use hermes_core::message::{Content, Message, Role};
use hermes_core::provider::Provider;
use hermes_core::tool::{ToolContext, ToolPermissions};
use tokio_util::sync::CancellationToken;

use crate::config::{HermesConfig, ProviderKind};
use crate::prompting::{
    build_runtime_system_prompt, compose_base_system_prompt, inject_system_prompt,
    resolve_skills_dir,
};
use crate::provider_factory::build_provider;
use crate::session::SessionContext;
use crate::tool_catalog::build_registry;

pub struct AIAgent {
    loop_: AgentLoop,
    base_system_prompt: Option<String>,
    provider_name: Option<String>,
}

impl AIAgent {
    pub fn from_config(config: HermesConfig) -> anyhow::Result<Self> {
        let provider_name = Some(provider_name(&config.provider).to_string());
        let base_system_prompt = compose_base_system_prompt(config.agent.system_prompt.as_deref());
        let provider = build_provider(&config.provider)?;
        Ok(Self {
            loop_: build_loop(Arc::from(provider), &config),
            base_system_prompt,
            provider_name,
        })
    }

    pub fn new(provider: impl Provider + 'static, config: HermesConfig) -> Self {
        let provider_name = Some(provider_name(&config.provider).to_string());
        let base_system_prompt = compose_base_system_prompt(config.agent.system_prompt.as_deref());
        Self {
            loop_: build_loop(Arc::new(provider), &config),
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
    ) -> Result<RunResult, hermes_core::LoopError> {
        self.run_messages(
            vec![Message {
                role: Role::User,
                content: Content::Text(user_text.to_string()),
                reasoning: None,
                tool_call_id: None,
                tool_calls: None,
            }],
            session,
            cancel,
            on_event,
        )
        .await
    }

    pub async fn run_messages(
        &self,
        messages: Vec<Message>,
        session: &SessionContext,
        cancel: CancellationToken,
        on_event: impl FnMut(LoopEvent) + Send,
    ) -> Result<RunResult, hermes_core::LoopError> {
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
}

fn build_loop(provider: Arc<dyn Provider>, config: &HermesConfig) -> AgentLoop {
    let skills_dir = resolve_skills_dir().unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".perry_hermes")
            .join("skills")
    });
    let registry = Arc::new(build_registry(&config.agent.disabled_toolsets, &skills_dir));
    AgentLoop::from_provider(
        provider,
        registry,
        LoopConfig {
            max_iterations: config.agent.max_iterations.unwrap_or(10),
            system_prompt: None,
            ..Default::default()
        },
    )
}

fn provider_name(config: &crate::config::ProviderConfig) -> &'static str {
    match config.kind {
        ProviderKind::Echo => "echo",
        ProviderKind::Openai => "openai",
        ProviderKind::Anthropic => "anthropic",
    }
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

    use crate::config::{ProviderConfig, ProviderKind};

    fn echo_config() -> HermesConfig {
        HermesConfig {
            provider: ProviderConfig {
                kind: ProviderKind::Echo,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn from_config_succeeds_for_echo_provider() {
        let agent =
            AIAgent::from_config(echo_config()).expect("echo should build with no env vars");
        drop(agent);
    }

    #[test]
    fn from_config_errors_on_missing_model() {
        let config = HermesConfig {
            provider: ProviderConfig {
                kind: ProviderKind::Openai,
                model: None,
                base_url: Some("https://api.openai.com/v1".into()),
                ..Default::default()
            },
            ..Default::default()
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
            provider: ProviderConfig {
                kind: ProviderKind::Openai,
                model: Some("gpt-4o-mini".into()),
                base_url: None,
                ..Default::default()
            },
            ..Default::default()
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
            provider: ProviderConfig {
                kind: ProviderKind::Openai,
                api_key_env: Some("HERMES_TEST_DEFINITELY_NOT_SET_98765".into()),
                model: Some("gpt-4o-mini".into()),
                base_url: Some("https://api.openai.com/v1".into()),
                ..Default::default()
            },
            ..Default::default()
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
            provider: ProviderConfig {
                kind: ProviderKind::Anthropic,
                model: None,
                base_url: Some("https://api.anthropic.com/v1".into()),
                ..Default::default()
            },
            ..Default::default()
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
