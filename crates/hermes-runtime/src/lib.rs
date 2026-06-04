//! `hermes-runtime` — the user-facing facade.
//!
//! The runtime composes the loop, providers, and tools behind a
//! simple `AIAgent` type. Phase 3 minimum: register a `BashTool` and
//! an `OpenAiProvider`, hand a `Vec<Message>` to `run()`, watch the
//! LLM call the tool. The full facade (session management, permissions,
//! budget, etc.) lands in later phases.
//!
//! See `plans/rust-port-design.md` §5 for the crate layout.

use std::sync::Arc;

use hermes_core::message::{Content, Message, Role};
use hermes_core::registry::InMemoryRegistry;
use hermes_core::tool::ToolContext;
use hermes_loop::{AgentLoop, LoopConfig, LoopEvent, RunResult};
use hermes_providers::OpenAiProvider;
use hermes_tools::BashTool;
use tokio_util::sync::CancellationToken;

/// The product API. Wires a provider, a toolset, and the loop
/// together. `run_turn()` returns a `RunResult` with the full
/// trajectory and metrics; the CLI uses it for the REPL, tests use
/// it to assert on event streams.
pub struct AIAgent {
    loop_: AgentLoop<OpenAiProvider, InMemoryRegistry>,
}

impl AIAgent {
    /// Build an agent that talks to any OpenAI-compatible endpoint
    /// (api.openai.com, DeepSeek, MiniMax, Ollama, your own vllm)
    /// and runs the BashTool.
    pub fn openai_compatible(
        api_key: impl Into<String>,
        model: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        let provider = OpenAiProvider::new(api_key, model).with_base_url(base_url);
        let registry = Arc::new(InMemoryRegistry::new().register(Arc::new(BashTool::new())));
        let loop_ = AgentLoop::new(
            provider,
            registry,
            LoopConfig {
                max_iterations: 20,
                system_prompt: Some(
                    "You are a careful assistant with access to a `bash` tool. \
                     Use it to inspect the system or run shell commands when \
                     needed. When you have enough information to answer, give \
                     a concise final response — do not call tools again."
                        .into(),
                ),
                ..Default::default()
            },
        );
        Self { loop_ }
    }

    /// Run a single user turn. Returns the full `RunResult` (final
    /// message + trajectory + metrics). The caller is responsible for
    /// printing the final message and rendering any side-channel
    /// events they care about.
    pub async fn run_turn(
        &self,
        user_text: &str,
        cancel: CancellationToken,
        mut on_event: impl FnMut(LoopEvent) + Send,
    ) -> Result<RunResult, hermes_core::LoopError> {
        let messages = vec![Message {
            role: Role::User,
            content: Content::Text(user_text.to_string()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        }];
        let ctx = ToolContext {
            session_id: "default".into(),
            working_dir: std::env::current_dir().unwrap_or_default(),
            permissions: Default::default(),
        };
        self.loop_.run(messages, ctx, cancel, &mut on_event).await
    }
}
