//! Runtime wiring shared by CLI and future gateways.

use std::path::PathBuf;
use std::sync::Arc;

pub mod config;

use anyhow::{anyhow, Context};
use config::{ProviderConfig, ProviderKind, ThinkingConfig, ThinkingMode};

use hermes_core::message::{Content, Message, Role};
use hermes_core::provider::Provider;
use hermes_core::registry::InMemoryRegistry;
use hermes_core::tool::{ToolContext, ToolPermissions};
use hermes_loop::{AgentLoop, LoopConfig, RunResult};
use hermes_providers::{
    AnthropicProvider, AnthropicRequestOptions, AnthropicThinking, EchoProvider, OpenAiProvider,
};
use hermes_tools::BashTool;
use tokio_util::sync::CancellationToken;

pub use config::HermesConfig;
pub use hermes_loop::LoopEvent;

pub const DEFAULT_SYSTEM_PROMPT: &str =
    "You are a careful assistant with access to a `bash` tool. \
Use it to inspect the system or run shell commands when needed. When you have enough information \
to answer, give a concise final response — do not call tools again.";

/// Per-run context that travels alongside the message list into `run_*`.
///
/// `HermesConfig` is the *static* configuration (provider, model, agent
/// limits). `SessionContext` is the *dynamic* per-invocation context
/// (which shell the agent is acting on behalf of, which directory to
/// start in). The runtime is reusable across sessions; the caller
/// supplies a fresh `SessionContext` for each `run_*` call.
#[derive(Debug, Clone)]
pub struct SessionContext {
    pub working_dir: PathBuf,
    pub session_id: String,
}

impl SessionContext {
    pub fn current_shell() -> Self {
        Self {
            working_dir: std::env::current_dir().unwrap_or_default(),
            session_id: "shell".into(),
        }
    }
}

pub struct AIAgent {
    loop_: AgentLoop,
}

impl AIAgent {
    /// Build an agent from a TOML-derived `HermesConfig`. The config
    /// determines the provider; `new` is the programmatic escape hatch
    /// for callers that already have a `Provider` in hand.
    pub fn from_config(config: HermesConfig) -> anyhow::Result<Self> {
        let provider = build_provider(&config.provider)?;
        // `build_provider` returns a `Box<dyn Provider>` so each match arm
        // can stay concise. `Arc<dyn Provider>` doesn't implement
        // `Provider` (no blanket impl), so we route through the loop
        // directly via `AgentLoop::from_provider`.
        Ok(Self {
            loop_: build_loop(Arc::from(provider), &config),
        })
    }

    /// Build an agent from a caller-supplied `Provider` and a
    /// `HermesConfig`. The `config.provider` field is ignored — only
    /// `config.agent` and `config.skills` shape the loop.
    pub fn new(provider: impl Provider + 'static, config: HermesConfig) -> Self {
        Self {
            loop_: build_loop(Arc::new(provider), &config),
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
        self.loop_.run(messages, ctx, cancel, on_event).await
    }
}

/// Shared loop construction for `AIAgent::from_config` and `AIAgent::new`.
/// Centralizes registry construction and `LoopConfig` initialization so
/// the two entry points stay in lockstep.
fn build_loop(provider: Arc<dyn Provider>, config: &HermesConfig) -> AgentLoop {
    let registry = Arc::new(build_registry(&config.agent.disabled_toolsets));
    let system_prompt = compose_system_prompt(config.agent.system_prompt.as_deref());
    AgentLoop::from_provider(
        provider,
        registry,
        LoopConfig {
            max_iterations: config.agent.max_iterations.unwrap_or(10),
            system_prompt,
            ..Default::default()
        },
    )
}

/// Compute the default skills directory from the user's `HOME`.
///
/// Returns `None` when `HOME` is unset. Matches the existing
/// `~/.perry_hermes/config.toml` convention used by the CLI.
fn default_skills_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".perry_hermes").join("skills"))
}

/// Compose the final system prompt: user-supplied prompt (or the
/// pre-unify default), plus a skills index block when skills exist.
///
/// Fixes the regression from `refactor(runtime): unify AIAgent API on
/// HermesConfig + SessionContext`, which made `DEFAULT_SYSTEM_PROMPT`
/// dead code. When the user does not supply a system prompt, this
/// function falls back to that default.
fn compose_system_prompt(user_prompt: Option<&str>) -> Option<String> {
    let skills = match default_skills_dir() {
        Some(d) => match hermes_skills::load_all(&d) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "failed to scan skills dir {}: {e}",
                    d.display()
                );
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    let skills_block = hermes_skills::render_system_prompt_block(&skills);

    let base = user_prompt.unwrap_or(DEFAULT_SYSTEM_PROMPT);

    if skills_block.is_empty() {
        Some(base.to_string())
    } else {
        Some(format!("{base}\n\n{skills_block}"))
    }
}

fn build_provider(config: &ProviderConfig) -> anyhow::Result<Box<dyn Provider>> {
    match config.kind {
        ProviderKind::Echo => Ok(Box::new(EchoProvider::new())),

        ProviderKind::Openai => {
            let model = config
                .model
                .as_deref()
                .ok_or_else(|| anyhow!("[provider].model is required for kind=openai"))?;
            let base_url = config
                .base_url
                .as_deref()
                .ok_or_else(|| anyhow!("[provider].base_url is required for kind=openai"))?;
            let api_key_env = config.api_key_env.as_deref().unwrap_or("OPENAI_API_KEY");
            let api_key = std::env::var(api_key_env)
                .with_context(|| format!("{api_key_env} is not set. Export it or set [provider].api_key_env in your config."))?;
            Ok(Box::new(
                OpenAiProvider::new(api_key, model).with_base_url(base_url),
            ))
        }

        ProviderKind::Anthropic => {
            let model = config
                .model
                .as_deref()
                .ok_or_else(|| anyhow!("[provider].model is required for kind=anthropic"))?;
            let base_url = config
                .base_url
                .as_deref()
                .ok_or_else(|| anyhow!("[provider].base_url is required for kind=anthropic"))?;
            let api_key_env = config.api_key_env.as_deref().unwrap_or("ANTHROPIC_API_KEY");
            let api_key = std::env::var(api_key_env)
                .with_context(|| format!("{api_key_env} is not set. Export it or set [provider].api_key_env in your config."))?;
            let api_key_header = config
                .api_key_header
                .clone()
                .unwrap_or_else(|| "x-api-key".into());
            let request_options = anthropic_request_options(config.thinking.as_ref());
            Ok(Box::new(
                AnthropicProvider::new(api_key, model)
                    .with_base_url(base_url)
                    .with_api_key_header(api_key_header)
                    .with_request_options(request_options),
            ))
        }
    }
}

fn anthropic_request_options(thinking: Option<&ThinkingConfig>) -> AnthropicRequestOptions {
    let resolved = thinking.and_then(|t| match t.mode {
        ThinkingMode::Off => None,
        ThinkingMode::Manual => Some(AnthropicThinking::Manual {
            budget_tokens: t.budget_tokens.unwrap_or(8_000),
        }),
        ThinkingMode::Adaptive => Some(AnthropicThinking::Adaptive {
            display: t.display.clone().unwrap_or_else(|| "summarized".into()),
            effort: t.effort.clone(),
        }),
    });
    AnthropicRequestOptions { thinking: resolved }
}

fn build_registry(disabled_toolsets: &[String]) -> InMemoryRegistry {
    if disabled_toolsets
        .iter()
        .any(|s| s == "core" || s == "terminal")
    {
        InMemoryRegistry::new()
    } else {
        InMemoryRegistry::new().register(Arc::new(BashTool::new()))
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
        let agent = AIAgent::from_config(echo_config()).expect("echo should build with no env vars");
        // Construction is the assertion. (We do not run the loop here.)
        drop(agent);
    }

    #[test]
    fn from_config_errors_on_missing_model() {
        let config = HermesConfig {
            provider: ProviderConfig {
                kind: ProviderKind::Openai,
                model: None, // missing
                base_url: Some("https://api.openai.com/v1".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let err = AIAgent::from_config(config)
            .err()
            .expect("expected from_config to fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("model"), "error should name the missing field: {msg}");
    }

    #[test]
    fn from_config_errors_on_missing_base_url() {
        let config = HermesConfig {
            provider: ProviderConfig {
                kind: ProviderKind::Openai,
                model: Some("gpt-4o-mini".into()),
                base_url: None, // missing
                ..Default::default()
            },
            ..Default::default()
        };
        let err = AIAgent::from_config(config)
            .err()
            .expect("expected from_config to fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("base_url"), "error should name the missing field: {msg}");
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
    fn new_with_custom_provider_and_default_config() {
        // AIAgent::new must work with HermesConfig::default() (used by the
        // example, and by callers that want to bring their own provider).
        use hermes_providers::EchoProvider;
        let agent = AIAgent::new(EchoProvider::new(), HermesConfig::default());
        drop(agent);
    }

    // --- SessionContext plumbing test ---------------------------------------

    /// Provider that emits exactly one tool call on the first call, then a
    /// Stop on the second. The tool is a `CaptureTool` below that records
    /// the `ToolContext` it received. We use a counter so the loop can
    /// exit cleanly after the tool runs once — the loop would otherwise
    /// keep asking and hit `max_iterations`.
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
        fn name(&self) -> &str { "capture" }
        fn description(&self) -> &str { "test tool that captures ToolContext" }
        fn parameters_schema(&self) -> Value { json!({"type": "object", "properties": {}}) }
        fn toolset(&self) -> &'static str { "core" }
        async fn execute(
            &self,
            _args: Value,
            ctx: ToolContext,
            _cancel: CancellationToken,
        ) -> Result<ToolOutput, ToolError> {
            *self.captured.lock().unwrap() = Some(ctx);
            Ok(ToolOutput { content: "ok".into() })
        }
    }

    #[tokio::test]
    async fn session_context_is_plumbed_into_tool_context() {
        let captured: Arc<Mutex<Option<ToolContext>>> = Arc::new(Mutex::new(None));

        let registry = InMemoryRegistry::new().register(Arc::new(CaptureTool {
            captured: Arc::clone(&captured),
        }));

        // We construct the loop directly here because the public
        // `AIAgent::new` registers `BashTool` (or an empty registry) and
        // we need to inject a custom `CaptureTool` to observe the
        // `ToolContext` it receives. The two public-API construction
        // paths are covered by `new_with_custom_provider_and_default_config`
        // and `from_config_succeeds_for_echo_provider` above. This test
        // specifically exercises the `SessionContext` → `ToolContext`
        // plumbing inside `AIAgent::run_turn` / `run_messages`.
        let provider = OneToolCallProvider {
            calls: Arc::new(Mutex::new(0)),
        };
        let loop_ = hermes_loop::AgentLoop::new(
            provider,
            Arc::new(registry),
            hermes_loop::LoopConfig {
                max_iterations: 3,
                ..Default::default()
            },
        );
        let agent = AIAgent { loop_ };

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
        assert_eq!(ctx.working_dir, std::path::PathBuf::from("/tmp/hermes-test-cwd"));
        assert_eq!(ctx.session_id, "session-xyz");
    }
}
