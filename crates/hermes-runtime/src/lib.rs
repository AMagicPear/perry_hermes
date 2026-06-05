//! Runtime wiring shared by CLI and future gateways.

use std::path::PathBuf;
use std::sync::Arc;

use hermes_core::message::{Content, Message, Role};
use hermes_core::provider::Provider;
use hermes_core::registry::InMemoryRegistry;
use hermes_core::tool::{ToolContext, ToolPermissions};
use hermes_loop::{AgentLoop, LoopConfig, RunResult};
use hermes_providers::{EchoProvider, OpenAiProvider};
use hermes_tools::BashTool;
use tokio_util::sync::CancellationToken;

pub use hermes_loop::LoopEvent;

pub const DEFAULT_SYSTEM_PROMPT: &str =
    "You are a careful assistant with access to a `bash` tool. \
Use it to inspect the system or run shell commands when needed. When you have enough information \
to answer, give a concise final response — do not call tools again.";

pub struct AgentOptions {
    pub max_iterations: u32,
    pub system_prompt: Option<String>,
    pub disabled_toolsets: Vec<String>,
    pub working_dir: PathBuf,
    pub session_id: String,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            max_iterations: 90,
            system_prompt: Some(DEFAULT_SYSTEM_PROMPT.into()),
            disabled_toolsets: Vec::new(),
            working_dir: std::env::current_dir().unwrap_or_default(),
            session_id: "default".into(),
        }
    }
}

pub struct AIAgent {
    loop_: AgentLoop,
    working_dir: PathBuf,
    session_id: String,
}

impl AIAgent {
    pub fn openai_compatible(
        api_key: impl Into<String>,
        model: impl Into<String>,
        base_url: impl Into<String>,
        options: AgentOptions,
    ) -> Self {
        Self::new(
            OpenAiProvider::new(api_key, model).with_base_url(base_url),
            options,
        )
    }

    pub fn echo(options: AgentOptions) -> Self {
        Self::new(EchoProvider::new(), options)
    }

    pub fn new(provider: impl Provider + 'static, options: AgentOptions) -> Self {
        let registry = build_registry(&options.disabled_toolsets);
        let loop_ = AgentLoop::new(
            provider,
            Arc::new(registry),
            LoopConfig {
                max_iterations: options.max_iterations,
                system_prompt: options.system_prompt,
                ..Default::default()
            },
        );
        Self {
            loop_,
            working_dir: options.working_dir,
            session_id: options.session_id,
        }
    }

    pub async fn run_messages(
        &self,
        messages: Vec<Message>,
        cancel: CancellationToken,
        on_event: impl FnMut(LoopEvent) + Send,
    ) -> Result<RunResult, hermes_core::LoopError> {
        self.loop_
            .run(messages, self.tool_context(), cancel, on_event)
            .await
    }

    pub async fn run_turn(
        &self,
        user_text: &str,
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
            cancel,
            on_event,
        )
        .await
    }

    fn tool_context(&self) -> ToolContext {
        ToolContext {
            session_id: self.session_id.clone(),
            working_dir: self.working_dir.clone(),
            permissions: ToolPermissions { subprocess: true },
        }
    }
}

pub fn build_registry(disabled_toolsets: &[String]) -> InMemoryRegistry {
    if disabled_toolsets
        .iter()
        .any(|s| s == "core" || s == "terminal")
    {
        InMemoryRegistry::new()
    } else {
        InMemoryRegistry::new().register(Arc::new(BashTool::new()))
    }
}
