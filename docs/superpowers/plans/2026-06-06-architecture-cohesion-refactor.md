# Architecture Cohesion Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Tighten crate boundaries so `hermes-core` is transport-agnostic, `hermes-runtime` becomes a thin assembly surface, provider/tool construction logic moves behind more cohesive module boundaries, and over-fragmented or over-tested areas are simplified rather than preserved mechanically.

**Architecture:** Preserve the existing workspace shape while refactoring internals in small steps. First remove the `reqwest` dependency leak from `hermes-core`, then extract cohesive runtime modules, then move provider and tool construction helpers behind narrower seams without changing top-level behavior. Keep each responsibility local instead of scattering one feature across many files, and treat redundant tests as refactor targets rather than untouchable assets.

**Tech Stack:** Rust workspace, Cargo tests, `reqwest`, `tokio`, `httpmock`, `tempfile`

---

## Execution Notes

- Implement inline in this session on a dedicated git branch, not in a worktree.
- Prefer fewer, higher-signal tests over repetitive coverage that locks in implementation details.
- When extracting modules, keep one capability gathered in one obvious place; do not split a single behavior across multiple files unless the dependency boundary genuinely requires it.

### Task 1: Remove transport coupling from `hermes-core`

**Files:**
- Modify: `crates/hermes-core/src/error.rs`
- Modify: `crates/hermes-core/Cargo.toml`
- Modify: `crates/hermes-providers/src/openai.rs`
- Modify: `crates/hermes-providers/src/anthropic.rs`
- Modify: `crates/hermes-providers/tests/openai.rs`
- Modify: `crates/hermes-providers/tests/anthropic.rs`
- Modify: `crates/hermes-providers/tests/openai_stream.rs`

- [ ] **Step 1: Write the failing transport-boundary regression test**

Add this test near the existing transport-error assertions in `crates/hermes-providers/tests/openai.rs`:

```rust
#[tokio::test]
async fn openai_provider_transport_error_is_transport_agnostic() {
    let provider = OpenAiProvider::new("k", "gpt-4o-mini")
        .with_base_url("http://127.0.0.1:1/v1");
    let cancel = CancellationToken::new();

    let err = match provider.stream(&[user_message("hi")], &[], cancel).await {
        Err(e) => e,
        Ok(_) => panic!("expected transport error, got Ok"),
    };

    match err {
        ProviderError::Transport(msg) => {
            assert!(!msg.is_empty(), "transport error should keep context");
        }
        other => panic!("expected Transport(String), got {other:?}"),
    }
}
```

- [ ] **Step 2: Run the targeted test to verify it fails**

Run: `cargo test -p hermes-providers openai_provider_transport_error_is_transport_agnostic -- --exact`

Expected: FAIL because `ProviderError::Transport` still wraps `reqwest::Error`, so the match arm or assertion will not compile or will mismatch.

- [ ] **Step 3: Make `ProviderError::Transport` transport-agnostic**

Update `crates/hermes-core/src/error.rs`:

```rust
/// Errors that can occur when calling an LLM provider.
#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("rate limited (retry after {retry_after_secs}s)")]
    RateLimited { retry_after_secs: u64 },
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}
```

Update `crates/hermes-core/Cargo.toml` to remove the `reqwest.workspace = true` dependency and the comment that justified it.

Update the error mapping call sites in `crates/hermes-providers/src/openai.rs` and `crates/hermes-providers/src/anthropic.rs`:

```rust
.send() => r.map_err(|e| ProviderError::Transport(e.to_string()))?,
```

and in any SSE parsing branches:

```rust
Err(e) => {
    yield Err(ProviderError::Transport(e.to_string()));
    return;
}
```

- [ ] **Step 4: Update provider transport assertions to the new shape**

Adjust any provider tests that currently assert:

```rust
assert!(matches!(result, ProviderError::Transport(_)));
```

to continue matching the new `String` payload without depending on `reqwest::Error`.

If `openai.rs`, `anthropic.rs`, and `openai_stream.rs` are asserting the same transport shape in multiple near-identical ways, collapse them to one high-signal transport test per provider rather than preserving duplicate coverage.

- [ ] **Step 5: Run focused provider/core verification**

Run: `cargo test -p hermes-core`

Expected: PASS

Run: `cargo test -p hermes-providers openai_provider_transport_error_is_transport_agnostic -- --exact`

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-core/src/error.rs crates/hermes-core/Cargo.toml crates/hermes-providers/src/openai.rs crates/hermes-providers/src/anthropic.rs crates/hermes-providers/tests/openai.rs crates/hermes-providers/tests/anthropic.rs
git commit -m "refactor(core): remove reqwest transport leak"
```

### Task 2: Split `hermes-runtime` into cohesive internal modules

**Files:**
- Create: `crates/hermes-runtime/src/agent.rs`
- Create: `crates/hermes-runtime/src/prompting.rs`
- Modify: `crates/hermes-runtime/src/lib.rs`
- Test: `crates/hermes-runtime/tests/skills_injection.rs`

- [ ] **Step 1: Write the failing runtime-structure regression test**

Add this test to `crates/hermes-runtime/tests/skills_injection.rs`:

```rust
#[tokio::test]
async fn runtime_new_preserves_user_prompt_without_skills_dir() {
    unsafe { std::env::remove_var("HOME") };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let mut config = config_for_echo();
    config.agent.system_prompt = Some("ONLY-CUSTOM".into());
    let agent = AIAgent::new(provider, config);

    let session = SessionContext {
        working_dir: PathBuf::from("/tmp"),
        session_id: "t".into(),
    };
    agent
        .run_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let system = msgs.iter().find(|m| m.role == hermes_core::message::Role::System).unwrap();
    let text = match &system.content {
        hermes_core::message::Content::Text(s) => s.clone(),
        _ => panic!("system message should be text"),
    };
    assert_eq!(text, "ONLY-CUSTOM");
}
```

- [ ] **Step 2: Run the targeted test to verify it currently fails or protects behavior**

Run: `cargo test -p hermes-runtime runtime_new_preserves_user_prompt_without_skills_dir -- --exact`

Expected: PASS or FAIL depending on existing behavior. If it passes, keep it as the guardrail before file moves. The important part is that the test exists before refactoring.

- [ ] **Step 3: Extract `agent.rs` and `prompting.rs`**

Create `crates/hermes-runtime/src/agent.rs` with the execution-facing logic only. Keep construction and assembly out of this file so runtime behavior stays cohesive instead of spreading one capability across assembly and execution modules.

Create `crates/hermes-runtime/src/agent.rs` with:

```rust
use std::path::PathBuf;
use std::sync::Arc;

use hermes_core::message::{Content, Message, Role};
use hermes_core::provider::Provider;
use hermes_core::tool::{ToolContext, ToolPermissions};
use hermes_loop::{AgentLoop, RunResult};
use tokio_util::sync::CancellationToken;

use crate::LoopEvent;

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
    pub(crate) loop_: AgentLoop,
}

impl AIAgent {
    pub fn new(provider: impl Provider + 'static, loop_: AgentLoop) -> Self {
        let _ = provider;
        Self { loop_ }
    }

    pub fn from_loop(loop_: AgentLoop) -> Self {
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
```

Create `crates/hermes-runtime/src/prompting.rs` with the extracted prompt helpers:

```rust
use std::path::PathBuf;

pub const DEFAULT_SYSTEM_PROMPT: &str =
    "You are a careful assistant with access to a `bash` tool. \
Use it to inspect the system or run shell commands when needed. When you have enough information \
to answer, give a concise final response — do not call tools again.";

fn default_skills_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".perry_hermes").join("skills"))
}

pub fn compose_system_prompt(user_prompt: Option<&str>) -> Option<String> {
    let skills = match default_skills_dir() {
        Some(d) => match hermes_skills::load_all(&d) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("failed to scan skills dir {}: {e}", d.display());
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
```

- [ ] **Step 4: Reduce `lib.rs` to a public surface plus assembly**

Update `crates/hermes-runtime/src/lib.rs` to:

```rust
pub mod agent;
pub mod config;
mod prompting;

pub use agent::{AIAgent, SessionContext};
pub use config::{AgentConfig, HermesConfig, ProviderConfig, ProviderKind};
pub use hermes_loop::LoopEvent;
pub use prompting::DEFAULT_SYSTEM_PROMPT;
```

Then move the existing `AIAgent::from_config` implementation in a way that preserves `AIAgent::new(provider, config)` behavior. The end state should keep `lib.rs` as the assembly entry point and let `agent.rs` own only run-time behavior, so one concern is not split across multiple files.

- [ ] **Step 5: Run focused runtime verification**

Run: `cargo test -p hermes-runtime runtime_new_preserves_user_prompt_without_skills_dir -- --exact`

Expected: PASS

Run: `cargo test -p hermes-runtime --test skills_injection`

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-runtime/src/lib.rs crates/hermes-runtime/src/agent.rs crates/hermes-runtime/src/prompting.rs crates/hermes-runtime/tests/skills_injection.rs
git commit -m "refactor(runtime): split agent and prompting modules"
```

### Task 3: Move provider construction behind a focused factory boundary

**Files:**
- Create: `crates/hermes-providers/src/factory.rs`
- Modify: `crates/hermes-providers/src/lib.rs`
- Modify: `crates/hermes-runtime/src/lib.rs`
- Modify: `crates/hermes-runtime/src/config.rs`
- Test: `crates/hermes-runtime/src/lib.rs`

- [ ] **Step 1: Write the failing provider-factory regression test**

Add this test near the existing `from_config_*` tests in `crates/hermes-runtime/src/lib.rs`:

```rust
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

    let err = AIAgent::from_config(config).err().expect("expected failure");
    let msg = format!("{err:#}");
    assert!(msg.contains("model"));
}
```

- [ ] **Step 2: Run the targeted test to verify the baseline**

Run: `cargo test -p hermes-runtime from_config_errors_on_missing_anthropic_model -- --exact`

Expected: PASS or FAIL. Keep the test as the guardrail before moving factory logic.

- [ ] **Step 3: Add provider factory support to `hermes-providers`**

Create `crates/hermes-providers/src/factory.rs`:

```rust
use anyhow::{anyhow, Context};
use hermes_core::Provider;
use hermes_runtime::config::{ProviderConfig, ProviderKind, ThinkingConfig, ThinkingMode};

use crate::{
    AnthropicProvider, AnthropicRequestOptions, AnthropicThinking, EchoProvider, OpenAiProvider,
};

pub fn build_provider(config: &ProviderConfig) -> anyhow::Result<Box<dyn Provider>> {
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
            let api_key = std::env::var(api_key_env).with_context(|| {
                format!("{api_key_env} is not set. Export it or set [provider].api_key_env in your config.")
            })?;
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
            let api_key = std::env::var(api_key_env).with_context(|| {
                format!("{api_key_env} is not set. Export it or set [provider].api_key_env in your config.")
            })?;
            let api_key_header = config
                .api_key_header
                .clone()
                .unwrap_or_else(|| "x-api-key".into());
            Ok(Box::new(
                AnthropicProvider::new(api_key, model)
                    .with_base_url(base_url)
                    .with_api_key_header(api_key_header)
                    .with_request_options(anthropic_request_options(config.thinking.as_ref())),
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
```

If the direct dependency on `hermes-runtime::config` creates a cycle, move the config structs into a neutral shared module inside `hermes-core` or define a small provider-factory input model owned outside runtime assembly, then adjust `hermes-runtime` to convert into it. Do not leave a circular dependency, and do not spread provider-construction rules between two different modules once the final location is chosen.

- [ ] **Step 4: Rewire runtime to use the provider factory**

Update `crates/hermes-providers/src/lib.rs`:

```rust
pub mod factory;
pub use factory::build_provider;
```

Update `crates/hermes-runtime/src/lib.rs` to remove the local `build_provider` and `anthropic_request_options` functions and instead call:

```rust
let provider = hermes_providers::build_provider(&config.provider)?;
```

- [ ] **Step 5: Run focused factory verification**

Run: `cargo test -p hermes-runtime from_config_errors_on_missing_anthropic_model -- --exact`

Expected: PASS

Run: `cargo test -p hermes-runtime`

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-providers/src/factory.rs crates/hermes-providers/src/lib.rs crates/hermes-runtime/src/lib.rs crates/hermes-runtime/src/config.rs
git commit -m "refactor(providers): extract provider factory"
```

### Task 4: Extract built-in tool registry construction from runtime assembly

**Files:**
- Create: `crates/hermes-runtime/src/tool_catalog.rs`
- Modify: `crates/hermes-runtime/src/lib.rs`
- Test: `crates/hermes-runtime/src/lib.rs`

- [ ] **Step 1: Write the failing tool-catalog regression test**

Add this test near the existing session/context tests in `crates/hermes-runtime/src/lib.rs`:

```rust
#[test]
fn runtime_disables_terminal_toolset_from_registry() {
    let registry = build_registry(&["terminal".to_string()]);
    let names: Vec<_> = registry.schemas().into_iter().map(|s| s.name).collect();
    assert!(!names.iter().any(|name| name == "bash"));
}
```

- [ ] **Step 2: Run the targeted test to verify the baseline**

Run: `cargo test -p hermes-runtime runtime_disables_terminal_toolset_from_registry -- --exact`

Expected: PASS or FAIL. Keep it as the guardrail before extraction.

- [ ] **Step 3: Extract the tool catalog helper**

Create `crates/hermes-runtime/src/tool_catalog.rs`:

```rust
use std::sync::Arc;

use hermes_core::registry::InMemoryRegistry;
use hermes_tools::BashTool;

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

Update `crates/hermes-runtime/src/lib.rs` to import and use `tool_catalog::build_registry`.

- [ ] **Step 4: Run focused catalog verification**

Run: `cargo test -p hermes-runtime runtime_disables_terminal_toolset_from_registry -- --exact`

Expected: PASS

Run: `cargo test -p hermes-runtime`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/hermes-runtime/src/tool_catalog.rs crates/hermes-runtime/src/lib.rs
git commit -m "refactor(runtime): extract tool catalog"
```

### Task 5: Run full verification and review boundary outcomes

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-06-06-architecture-cohesion-refactor-design.md`

- [ ] **Step 1: Update docs to match the new boundaries**

Adjust `README.md` and the approved design doc so they accurately describe:

- `hermes-core` no longer depends on `reqwest`
- `hermes-runtime` is split into focused modules
- provider and tool construction boundaries
- any meaningful test deletions that reduced duplication without reducing behavioral coverage

- [ ] **Step 2: Run full workspace verification**

Run: `cargo test`

Expected: PASS with the full workspace green.

- [ ] **Step 3: Run one more focused runtime verification after doc updates**

Run: `cargo test -p hermes-runtime`

Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add README.md docs/superpowers/specs/2026-06-06-architecture-cohesion-refactor-design.md
git commit -m "docs: refresh architecture boundaries"
```
