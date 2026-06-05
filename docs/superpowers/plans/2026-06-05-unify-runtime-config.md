# Unify Runtime Config — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collapse `HermesConfig` + `AgentOptions` into a single config, reduce `AIAgent` to two public constructors, drop env-var fallbacks from the runtime, and require a TOML config file at startup (looked up in a fixed order).

**Architecture:** Single `HermesConfig` is the only data type; per-run `working_dir`/`session_id` move into a new `SessionContext` that is passed to `run_*`. `AIAgent` exposes `from_config(HermesConfig) -> Result<Self>` and `new(provider, HermesConfig) -> Self`; provider-specific constructors are deleted. CLI resolves a config path through `--config` → `~/.perry_hermes/config.toml` → `./hermes.toml`; nothing else reads the environment.

**Tech Stack:** Rust (workspace already at edition 2021, rust 1.75), TOML via `toml` crate, `serde` derive, existing `hermes-core`/`hermes-loop`/`hermes-providers`/`hermes-tools` crates, `tokio` for tests.

**Spec:** `docs/superpowers/specs/2026-06-05-unify-runtime-config-design.md`

---

## File map

| File | Responsibility after this plan |
|---|---|
| `crates/hermes-runtime/src/config.rs` | Holds `HermesConfig`, `ProviderConfig`, `ProviderKind`, `ThinkingConfig`, `ThinkingMode`, `AgentConfig`, `SkillsConfig`. `ProviderConfig` and `ProviderKind` gain `Default`. No new code paths. |
| `crates/hermes-runtime/src/lib.rs` | Defines `AIAgent`, `SessionContext`, the two constructors (`from_config` / `new`), and the two `run_*` methods. Provider-specific constructors and `AgentOptions` are gone. Holds the `build_provider` helper that turns a `ProviderConfig` into a `Box<dyn Provider>` with explicit error messages. |
| `crates/hermes-runtime/examples/live_tool_use.rs` | Calls `AIAgent::new(OpenAiProvider::…, HermesConfig::default())` and passes a `SessionContext` to `run_turn`. Reads env vars itself; runtime no longer does. |
| `crates/hermes-cli/src/main.rs` | Two flags: `--config` (optional) and `--cwd` (optional). Has a private `resolve_config_path` helper implementing the lookup order. `dispatch` is 3 lines after that. `run_repl` takes `&SessionContext`. |
| `crates/hermes-cli/hermes.example.toml` | **New.** Sample TOML the user can copy to either default path. |
| `crates/hermes-cli/tests/cli_smoke.rs` | **New.** Spawns the `hermes` binary and asserts the no-config error, the default-path pickup, and `--config` override. |
| `crates/hermes-runtime/Cargo.toml` | Adds `[dev-dependencies]` for `async-trait`, `tokio` (macros), `futures`. |
| `crates/hermes-cli/Cargo.toml` | No new deps (test uses `std::process::Command` + `env!("CARGO_BIN_EXE_hermes")`). |
| `CLAUDE.md` | Updates the "Runtime + CLI" paragraph. |
| `docs/superpowers/specs/2026-06-05-phase-9-config-and-skills-design.md` | Adds a "Superseded by" note at the top. |

---

## Task 1: Add `Default` to `ProviderConfig` and `ProviderKind`

**Files:**
- Modify: `crates/hermes-runtime/src/config.rs:24-46`

- [ ] **Step 1: Add `Default` derives**

Edit `crates/hermes-runtime/src/config.rs`:

```rust
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct ProviderConfig {
    #[serde(default)]
    pub kind: ProviderKind,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_header: Option<String>,
    #[serde(default)]
    pub thinking: Option<ThinkingConfig>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    #[default]
    Echo,
    Openai,
    Anthropic,
}
```

- [ ] **Step 2: Verify build + tests pass**

Run: `cargo build -p hermes-runtime`
Expected: clean build, no warnings.

Run: `cargo test -p hermes-runtime`
Expected: the two existing tests in `config.rs` still pass (they use the original `ProviderConfig` shape; `Default` is additive).

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-runtime/src/config.rs
git commit -m "feat(runtime): add Default to ProviderConfig and ProviderKind"
```

---

## Task 2: Add `SessionContext` to the runtime

**Files:**
- Modify: `crates/hermes-runtime/src/lib.rs:1-19` (top of file)

This is a pure addition — `SessionContext` is defined but not yet wired into `AIAgent`. The build remains green; later tasks wire it in.

- [ ] **Step 1: Add the `SessionContext` type**

Edit `crates/hermes-runtime/src/lib.rs`. After the existing `pub use hermes_loop::LoopEvent;` line and before `pub const DEFAULT_SYSTEM_PROMPT`, insert:

```rust
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
    /// A `SessionContext` for "the current shell session in the current
    /// working directory." Useful for examples and single-shot tools.
    pub fn current_shell() -> Self {
        Self {
            working_dir: std::env::current_dir().unwrap_or_default(),
            session_id: "shell".into(),
        }
    }
}
```

- [ ] **Step 2: Verify build**

Run: `cargo build -p hermes-runtime`
Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-runtime/src/lib.rs
git commit -m "feat(runtime): add SessionContext for per-run context"
```

---

## Task 3: Refactor `AIAgent` — TDD red→green

**Files:**
- Modify: `crates/hermes-runtime/src/lib.rs` (full refactor)
- Modify: `crates/hermes-runtime/Cargo.toml` (add dev-deps for tests)

This task does both RED (write failing tests) and GREEN (implement) in one commit. The intermediate state inside the commit is broken-by-design; the final state compiles and all new tests pass.

- [ ] **Step 1: Add dev-deps to `crates/hermes-runtime/Cargo.toml`**

Append to the file:

```toml
[dev-dependencies]
async-trait.workspace = true
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
futures.workspace = true
hermes-providers.workspace = true
```

- [ ] **Step 2: Write failing tests**

Append to `crates/hermes-runtime/src/lib.rs` (right before the closing `}` of the file):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use futures::stream;
    use hermes_core::message::{Content, Message, Role};
    use hermes_core::provider::{
        CompletionDelta, CompletionStream, FinishReason, Provider, ProviderError, ToolCallDelta, Usage,
    };
    use hermes_core::registry::{InMemoryRegistry, ToolSchema};
    use hermes_core::tool::{Tool, ToolContext, ToolError, ToolOutput};
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
        let err = AIAgent::from_config(config).unwrap_err();
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
        let err = AIAgent::from_config(config).unwrap_err();
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
        let err = AIAgent::from_config(config).unwrap_err();
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

    /// Provider that emits exactly one tool call, then a Stop. The tool is
    /// a `CaptureTool` below that records the `ToolContext` it received.
    struct OneToolCallProvider;

    #[async_trait]
    impl Provider for OneToolCallProvider {
        async fn stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _cancel: CancellationToken,
        ) -> Result<CompletionStream, ProviderError> {
            let deltas = vec![
                CompletionDelta {
                    content_delta: None,
                    reasoning_delta: None,
                    tool_call_delta: Some(ToolCallDelta {
                        index: 0,
                        id: Some("call_1".into()),
                        name: Some("capture".into()),
                        arguments_delta: Some("{}".into()),
                    }),
                    usage: Some(Usage::default()),
                    finish_reason: None,
                },
                CompletionDelta {
                    content_delta: Some("done".into()),
                    reasoning_delta: None,
                    tool_call_delta: None,
                    usage: Some(Usage::default()),
                    finish_reason: Some(FinishReason::Stop),
                },
            ];
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

        let mut registry = InMemoryRegistry::new();
        registry.register(Arc::new(CaptureTool {
            captured: Arc::clone(&captured),
        }));

        let provider = OneToolCallProvider;
        let config = HermesConfig::default();
        let loop_ = hermes_loop::AgentLoop::new(
            provider,
            Arc::new(registry),
            hermes_loop::LoopConfig {
                max_iterations: 2,
                ..Default::default()
            },
        );
        let agent = AIAgent { loop_ };

        let session = SessionContext {
            working_dir: std::path::PathBuf::from("/tmp/hermes-test-cwd"),
            session_id: "session-xyz".into(),
        };

        let cancel = CancellationToken::new();
        let _ = agent
            .run_turn("hi", &session, cancel, |_| {})
            .await
            .expect("run should succeed");

        let ctx = captured.lock().unwrap().clone().expect("tool was called");
        assert_eq!(ctx.working_dir, std::path::PathBuf::from("/tmp/hermes-test-cwd"));
        assert_eq!(ctx.session_id, "session-xyz");
    }
}
```

- [ ] **Step 3: Run the new tests — they should FAIL TO COMPILE**

Run: `cargo test -p hermes-runtime --no-run 2>&1 | head -40`
Expected: errors like "cannot find struct `SessionContext` in this scope", "no method named `from_config` for type `AIAgent`", etc. The test file references symbols that don't exist yet. **This is the RED state — do not stop to fix it.**

- [ ] **Step 4: Refactor `AIAgent` — replace the entire `lib.rs` body**

Replace the entire contents of `crates/hermes-runtime/src/lib.rs` (everything except the `pub mod config;` line and the `#[cfg(test)] mod tests` we just added) with:

```rust
//! Runtime wiring shared by CLI and future gateways.

use std::path::PathBuf;
use std::sync::Arc;

pub mod config;

use anyhow::{anyhow, Context};
use config::{AgentConfig, HermesConfig, ProviderConfig, ProviderKind, ThinkingConfig, ThinkingMode};

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

pub use hermes_loop::LoopEvent;

pub const DEFAULT_SYSTEM_PROMPT: &str =
    "You are a careful assistant with access to a `bash` tool. \
Use it to inspect the system or run shell commands when needed. When you have enough information \
to answer, give a concise final response — do not call tools again.";

/// Per-run context. See `lib.rs` top-of-file doc comment on `SessionContext`.
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
        Ok(Self::new(provider, config))
    }

    /// Build an agent from a caller-supplied `Provider` and a
    /// `HermesConfig`. The `config.provider` field is ignored — only
    /// `config.agent` and `config.skills` shape the loop.
    pub fn new(provider: impl Provider + 'static, config: HermesConfig) -> Self {
        let registry = build_registry(&config.agent.disabled_toolsets);
        let loop_ = AgentLoop::new(
            provider,
            Arc::new(registry),
            LoopConfig {
                max_iterations: config.agent.max_iterations.unwrap_or(10),
                system_prompt: config.agent.system_prompt,
                ..Default::default()
            },
        );
        Self { loop_ }
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
```

(The `#[cfg(test)] mod tests { ... }` block from Step 2 stays at the bottom of the file.)

- [ ] **Step 5: Run the new tests — they should PASS**

Run: `cargo test -p hermes-runtime`
Expected: 6 new tests pass (5 `#[test]` + 1 `#[tokio::test]`), the 2 pre-existing config tests still pass, the example (`live_tool_use`) is broken — that's fixed in Task 4.

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-runtime/src/lib.rs crates/hermes-runtime/Cargo.toml
git commit -m "refactor(runtime): unify AIAgent API on HermesConfig + SessionContext"
```

---

## Task 4: Rewire `live_tool_use` example

**Files:**
- Modify: `crates/hermes-runtime/examples/live_tool_use.rs`

- [ ] **Step 1: Replace the example body**

The new file contents (only the changed parts shown — top `use` lines and the construction site):

```rust
use hermes_core::message::Content;
use hermes_core::LoopError;
use hermes_providers::OpenAiProvider;
use hermes_runtime::{AIAgent, HermesConfig, LoopEvent, SessionContext};
use tokio_util::sync::CancellationToken;
```

And in `main()`, replace the agent construction block (currently:

```rust
let agent = AIAgent::openai_compatible(&api_key, &model, &base_url, AgentOptions::default());
```

) with:

```rust
let provider = OpenAiProvider::new(&api_key, &model).with_base_url(&base_url);
let config = HermesConfig::default();
let session = SessionContext::current_shell();
let agent = AIAgent::new(provider, config);
```

And update the `run_turn` call to pass `&session`:

```rust
agent.run_turn(&user_text, &session, cancel, |event| match event {
```

(Find the existing `agent.run_turn(&user_text, cancel, …)` call and add `&session` as the second argument.)

- [ ] **Step 2: Verify build**

Run: `cargo build -p hermes-runtime --examples`
Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-runtime/examples/live_tool_use.rs
git commit -m "refactor(examples): rewire live_tool_use to unified AIAgent API"
```

---

## Task 5: Refactor the CLI

**Files:**
- Create: `crates/hermes-cli/hermes.example.toml`
- Modify: `crates/hermes-cli/src/main.rs` (full rewrite of `Args`, `dispatch`, `run_repl`)

- [ ] **Step 1: Add the example TOML**

Create `crates/hermes-cli/hermes.example.toml` with:

```toml
# Sample Hermes config.
#
# Copy to one of:
#   ./hermes.toml                 (project-local, per-cwd)
#   ~/.perry_hermes/config.toml   (user-global)
#
# Then run `hermes` with no flags. See
# docs/superpowers/specs/2026-06-05-unify-runtime-config-design.md.

[provider]
kind = "openai"  # openai | anthropic | echo
api_key_env = "OPENAI_API_KEY"
model = "gpt-4o-mini"
base_url = "https://api.openai.com/v1"

[agent]
max_iterations = 10
disabled_toolsets = []
# system_prompt = "..."   # optional full replacement of the default

[skills]
enabled = []
paths = ["./skills"]
```

- [ ] **Step 2: Replace `crates/hermes-cli/src/main.rs`**

The full new file:

```rust
//! Hermes CLI — interactive REPL for the Hermes agent.
//!
//! Reads `--config` (or falls back to `~/.perry_hermes/config.toml` then
//! `./hermes.toml`), constructs the runtime, and renders `LoopEvent`s.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context};
use clap::Parser;

use hermes_core::error::LoopError;
use hermes_core::message::{Content, Message, Role};
use hermes_runtime::{AIAgent, HermesConfig, LoopEvent, SessionContext};
use tokio_util::sync::CancellationToken;

mod ctrl_c;
use ctrl_c::{CtrlCAction, CtrlCHandler};

#[derive(Parser)]
#[command(
    name = "hermes",
    version,
    about = "Hermes — AI agent with tool use",
    long_about = None
)]
struct Args {
    /// Path to HermesConfig TOML. If omitted, the CLI looks in
    /// `~/.perry_hermes/config.toml` then `./hermes.toml`.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Working directory for the session (defaults to the process's cwd).
    #[arg(long)]
    cwd: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let tokio_rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    tokio_rt.block_on(async { dispatch(args).await })
}

async fn dispatch(args: Args) -> anyhow::Result<()> {
    let config_path = resolve_config_path(args.config.as_deref())?;
    let config = HermesConfig::from_path(&config_path)
        .with_context(|| format!("failed to load config from {}", config_path.display()))?;

    let session = SessionContext {
        working_dir: args
            .cwd
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default()),
        session_id: "cli".into(),
    };

    let agent = AIAgent::from_config(config)
        .with_context(|| format!("failed to build agent from {}", config_path.display()))?;

    run_repl(agent, &session).await
}

fn resolve_config_path(explicit: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(p) = explicit {
        if !p.exists() {
            bail!("--config {} does not exist", p.display());
        }
        return Ok(p.to_path_buf());
    }

    let mut tried = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home).join(".perry_hermes").join("config.toml");
        tried.push(p.clone());
        if p.exists() {
            return Ok(p);
        }
    }
    let cwd_default = PathBuf::from("hermes.toml");
    tried.push(cwd_default.clone());
    if cwd_default.exists() {
        return Ok(cwd_default);
    }

    let mut msg = String::from("no hermes config found. Looked for:\n");
    for p in &tried {
        msg.push_str(&format!("  - {}\n", p.display()));
    }
    msg.push_str("Pass --config <path> or create one of these. See crates/hermes-cli/hermes.example.toml for a starter.");
    bail!(msg);
}

async fn run_repl(agent: AIAgent, session: &SessionContext) -> anyhow::Result<()> {
    eprintln!(
        "hermes v{} — type a message, Ctrl-D to quit, Ctrl-C to cancel a turn or (when idle) quit",
        env!("CARGO_PKG_VERSION")
    );
    eprintln!();

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut history: Vec<Message> = Vec::new();

    let ctrl_c = Arc::new(CtrlCHandler::new());
    let ctrl_c_signal = Arc::clone(&ctrl_c);
    let signal_handle = tokio::spawn(async move {
        loop {
            if tokio::signal::ctrl_c().await.is_ok() {
                match ctrl_c_signal.handle() {
                    CtrlCAction::Exit => {
                        eprintln!();
                        std::process::exit(0);
                    }
                    CtrlCAction::Cancel => {}
                }
            }
        }
    });

    for line in stdin.lock().lines() {
        let line = line.context("failed to read line")?;
        let line = line.trim().to_string();

        if line == "/quit" || line == "/exit" {
            break;
        }
        if line.is_empty() {
            continue;
        }

        history.push(Message {
            role: Role::User,
            content: Content::Text(line),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        });

        let cancel = CancellationToken::new();
        ctrl_c.enter_turn(cancel.clone());

        let result = agent
            .run_messages(history.clone(), session, cancel.clone(), |event| match event {
                LoopEvent::Thinking => {
                    eprint!("… ");
                    let _ = stdout.flush();
                }
                LoopEvent::ToolCallStarted { call, .. } => {
                    let preview = truncate_str(&call.arguments.to_string(), 80);
                    eprint!("\n  📦 {}({})", call.name, preview);
                    let _ = stdout.flush();
                }
                LoopEvent::ToolCallFinished { call, result } => {
                    match &result {
                        Ok(out) => {
                            let preview = truncate_str(&out.content, 160);
                            eprint!("\n  ← {} {}", tool_emoji(&call.name), preview);
                        }
                        Err(e) => {
                            eprint!("\n  ← ❌ {e}");
                        }
                    }
                    let _ = stdout.flush();
                }
                LoopEvent::AssistantMessage(_) => {
                    eprintln!();
                }
                LoopEvent::LengthLimit => eprintln!("[hit length limit]"),
                LoopEvent::IterationsExhausted => eprintln!("[max iterations]"),
                LoopEvent::Cancelled => eprintln!("[cancelled]"),
                LoopEvent::ContentDelta(s) => {
                    eprint!("{s}");
                    let _ = stdout.flush();
                }
                LoopEvent::ReasoningDelta(s) => {
                    eprint!("\x1b[2m{s}\x1b[0m");
                    let _ = stdout.flush();
                }
                LoopEvent::ToolCallPartial(_) => {}
            })
            .await;

        ctrl_c.exit_turn();

        match result {
            Ok(run_result) => {
                let _ = &run_result.final_message;
                history = run_result.messages;

                eprintln!(
                    "  [iterations={} tool_calls={} in={} out={}]",
                    run_result.metrics.iterations,
                    run_result.metrics.tool_calls,
                    run_result.metrics.input_tokens,
                    run_result.metrics.output_tokens,
                );
                eprintln!();
            }
            Err(LoopError::CancelledWith(partial)) => {
                let chars = match &partial.content {
                    Content::Text(s) => s.chars().count(),
                    Content::Parts(_) => 0,
                };
                let calls = partial.tool_calls.as_ref().map(|c| c.len()).unwrap_or(0);
                eprintln!(
                    "\n  [cancelled mid-stream: {chars} chars streamed, {calls} tool call kept]"
                );
                if chars > 0 || calls > 0 {
                    history.push(partial);
                } else {
                    history.pop();
                }
                eprintln!();
            }
            Err(LoopError::Cancelled) => {
                eprintln!("\n[cancelled]");
                history.pop();
                eprintln!();
            }
            Err(e) => {
                eprintln!("error: {e}");
                history.pop();
                eprintln!();
            }
        }

        let _ = stdout.flush();
    }

    signal_handle.abort();
    Ok(())
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}…")
    }
}

fn tool_emoji(name: &str) -> &'static str {
    match name {
        "bash" | "terminal" => "⚡",
        "read_file" | "write_file" => "📄",
        "search_files" => "🔰",
        "memory" => "🧠",
        _ => "🔧",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes tests that mutate process-wide state (`HOME`). `cargo
    /// test` runs `#[test]` functions in parallel by default; without
    /// this, two tests setting `HOME` concurrently can observe each
    /// other's value. Locking the same `Mutex` (poisoning on panic is
    /// fine — we have no other invariants to protect) keeps them
    /// sequential. The integration tests in `tests/cli_smoke.rs` do
    /// not need this because each spawns a child process with HOME
    /// passed via `.env()`, never touching the test process's env.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Returns `(temp_home, cwd_dir)` with neither containing a config
    /// file. Each call produces a unique base directory (pid + nanos);
    /// leftover dirs under the system temp dir are harmless.
    fn make_empty_dirs() -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "hermes-cli-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = base.join("home");
        let cwd = base.join("cwd");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        (home, cwd)
    }

    #[test]
    fn resolve_explicit_path_must_exist() {
        let _guard = ENV_LOCK.lock().unwrap();
        let result = resolve_config_path(Some(Path::new("/does/not/exist.toml")));
        let err = result.unwrap_err().to_string();
        assert!(err.contains("/does/not/exist.toml"), "{err}");
    }

    #[test]
    fn resolve_picks_cwd_hermes_toml_when_no_home_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        let (home, cwd) = make_empty_dirs();
        let config_path = cwd.join("hermes.toml");
        std::fs::write(&config_path, "[provider]\nkind=\"echo\"\n").unwrap();

        unsafe { std::env::set_var("HOME", &home); }
        let result = resolve_config_path(None);
        unsafe { std::env::remove_var("HOME"); }

        let resolved = result.expect("should resolve to ./hermes.toml");
        assert_eq!(resolved, config_path);
    }

    #[test]
    fn resolve_errors_with_message_naming_all_tried_paths() {
        let _guard = ENV_LOCK.lock().unwrap();
        let (home, _cwd) = make_empty_dirs();
        unsafe { std::env::set_var("HOME", &home); }
        let result = resolve_config_path(None);
        unsafe { std::env::remove_var("HOME"); }

        let err = result.unwrap_err().to_string();
        assert!(err.contains("no hermes config found"), "{err}");
        assert!(err.contains(".perry_hermes"), "{err}");
        assert!(err.contains("hermes.toml"), "{err}");
    }
}
```

(We deliberately don't clean up `temp_home` aggressively — `make_empty_dirs` uses the process id + nanosecond timestamp in its path, so concurrent test runs don't collide, and leftover dirs are harmless.)

- [ ] **Step 3: Verify build + new unit tests pass**

Run: `cargo build -p hermes-cli`
Expected: clean build.

Run: `cargo test -p hermes-cli --bin hermes`
Expected: the 3 new `resolve_*` tests pass. (The 2 in `cfg(test)` are inline; we run only the bin's tests here because the integration tests are in a separate file added in Task 6.)

- [ ] **Step 4: Smoke-test the binary by hand**

Run: `cd /tmp && mkdir -p hermes-empty && cd hermes-empty && unset HOME && cargo run -p hermes-cli --quiet 2>&1 | head -5`
Expected: stderr contains "no hermes config found".

(Use a tempdir as cwd so `./hermes.toml` is absent too. If `HOME` is unset on your shell, fine; if not, point HOME at the same empty dir.)

- [ ] **Step 5: Commit**

```bash
git add crates/hermes-cli/hermes.example.toml crates/hermes-cli/src/main.rs
git commit -m "refactor(cli): collapse flags to --config/--cwd with default lookup"
```

---

## Task 6: Add the CLI integration test

**Files:**
- Create: `crates/hermes-cli/tests/cli_smoke.rs`

- [ ] **Step 1: Create the integration test file**

```rust
//! End-to-end smoke for the `hermes` binary's config resolution and
//! basic REPL round-trip with the `echo` provider.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Path to a fresh, empty scratch dir under the system temp dir.
fn scratch(label: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "hermes-cli-itest-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&p).unwrap();
    p
}

fn hermes_bin() -> &'static str {
    env!("CARGO_BIN_EXE_hermes")
}

#[test]
fn hermes_errors_when_no_config_is_found() {
    let home = scratch("nohome");
    let cwd = scratch("nocwd");
    fs::create_dir_all(home.join(".perry_hermes")).unwrap();

    let output = Command::new(hermes_bin())
        .env("HOME", &home)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to spawn hermes");

    assert!(!output.status.success(), "expected non-zero exit, got {:?}", output.status);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no hermes config found"),
        "stderr should explain the lookup failure, got: {stderr}"
    );
    assert!(stderr.contains(".perry_hermes"), "stderr should name the home path: {stderr}");
    assert!(stderr.contains("hermes.toml"), "stderr should name the cwd path: {stderr}");
}

#[test]
fn hermes_picks_up_cwd_hermes_toml() {
    let home = scratch("cwdhome"); // empty HOME so ~/.perry_hermes/config.toml is absent
    let cwd = scratch("cwdtoml");
    let config_path = cwd.join("hermes.toml");
    fs::write(&config_path, "[provider]\nkind=\"echo\"\n").unwrap();

    let mut child = Command::new(hermes_bin())
        .env("HOME", &home)
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn hermes");

    // Write one line, then close stdin so the REPL exits cleanly.
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"hello\n")
        .expect("write to stdin");
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("failed to wait on hermes");
    assert!(
        output.status.success(),
        "expected zero exit on EOF, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    // Echo provider echoes back "echo: hello"; the REPL streams it via
    // ContentDelta to stdout (per the on_event closure in main.rs).
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("echo: hello"),
        "stdout should contain the echoed response, got: {stdout}"
    );
}

#[test]
fn hermes_respects_explicit_config_flag() {
    let home = scratch("flaghome"); // empty HOME
    let cwd = scratch("flagcwd");   // empty cwd
    let config_dir = scratch("flagcfg");
    let config_path = config_dir.join("my-config.toml");
    fs::write(
        &config_path,
        "[provider]\nkind=\"echo\"\n[agent]\nmax_iterations=2\n",
    )
    .unwrap();

    let mut child = Command::new(hermes_bin())
        .env("HOME", &home)
        .current_dir(&cwd)
        .arg("--config")
        .arg(&config_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn hermes");

    child.stdin.as_mut().unwrap().write_all(b"hi\n").unwrap();
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("failed to wait on hermes");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}
```

- [ ] **Step 2: Run the integration tests**

Run: `cargo test -p hermes-cli --test cli_smoke`
Expected: 3 tests pass. (Each spawns the actual `hermes` binary built by `cargo test`.)

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-cli/tests/cli_smoke.rs
git commit -m "test(cli): add cli_smoke integration covering config resolution"
```

---

## Task 7: Update docs

**Files:**
- Modify: `CLAUDE.md`
- Modify: `docs/superpowers/specs/2026-06-05-phase-9-config-and-skills-design.md`

- [ ] **Step 1: Update `CLAUDE.md`**

In the "Runtime + CLI" paragraph (the bullet starting "Runtime is the shared composition point..."), change the sentence:

> `AIAgent` builds the provider, registry, loop, `ToolContext`, and system prompt. CLI only parses args, maintains REPL history, and renders `LoopEvent`s. Multi-turn: `run_result.messages` becomes the next turn's `history`. Ctrl-C: first cancels the current turn via `CancellationToken`, second exits the loop. Slash commands: `/quit`, `/exit`. `--disabled-toolsets core|terminal` is passed to runtime.

to:

> `AIAgent::from_config(HermesConfig)` and `AIAgent::new(provider, HermesConfig)` are the two constructors; runtime builds the registry, loop, and resolves the `Provider` from the config. Per-run `working_dir` / `session_id` travel in a `SessionContext` passed to `run_turn` / `run_messages` (the runtime is reusable across sessions). CLI only parses args, resolves the config path (`--config` → `~/.perry_hermes/config.toml` → `./hermes.toml`, error if none), maintains REPL history, and renders `LoopEvent`s. Multi-turn: `run_result.messages` becomes the next turn's `history`. Ctrl-C: first cancels the current turn via `CancellationToken`, second exits the loop. Slash commands: `/quit`, `/exit`. Provider-specific things (`OPENAI_API_KEY` etc.) live in `[provider]` of the TOML file; the runtime never reads the environment for defaults.

- [ ] **Step 2: Add a "Superseded by" note to the phase-9 spec**

Edit `docs/superpowers/specs/2026-06-05-phase-9-config-and-skills-design.md`. At the very top, after the title, insert:

```markdown
> **Superseded by** `2026-06-05-unify-runtime-config-design.md`. The CLI now requires a config file (looked up in `~/.perry_hermes/config.toml` → `./hermes.toml`); environment-variable fallbacks (`OPENAI_MODEL`, `ANTHROPIC_BASE_URL`, etc.) and provider-specific CLI flags (`--provider`, `--model`, `--base-url`, `--max-iterations`, `--disabled-toolsets`) are removed. `HermesConfig` is the single source of truth; `AIAgent` exposes only `from_config` and `new`; per-run `working_dir`/`session_id` travel in a `SessionContext` parameter.
```

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md docs/superpowers/specs/2026-06-05-phase-9-config-and-skills-design.md
git commit -m "docs: update CLAUDE.md and supersede phase-9 env-fallback bits"
```

---

## Self-review checklist (run before declaring done)

- [ ] `cargo build --workspace` — clean.
- [ ] `cargo test --workspace` — all green, including the new `cli_smoke` integration tests.
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` — clean.
- [ ] `grep -rn "AgentOptions\|AIAgent::openai_compatible\|AIAgent::anthropic\|AIAgent::echo\|merge_agent_options" crates/` — no hits (all deleted).
- [ ] `grep -rn "apply_cli_overrides\|args.provider" crates/hermes-cli/src/main.rs` — no hits.
- [ ] Manual smoke: `cd /tmp && mkdir -p hermes-smoke && cd hermes-smoke && cp <repo>/crates/hermes-cli/hermes.example.toml hermes.toml && cargo run -p hermes-cli --quiet < /dev/null` exits 0 with the "hermes" banner.
