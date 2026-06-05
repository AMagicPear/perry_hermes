# Phase 9 Config File Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a TOML config file path for provider and agent settings, and make Anthropic thinking explicit instead of inferred from model names.

**Architecture:** Keep config loading in `hermes-runtime`, where provider construction already lives. CLI reads `--config`, merges command-line overrides over config values, and then builds `AIAgent` through config-aware runtime constructors. Skills are intentionally not implemented in this plan; they remain a follow-up feature that will reuse the same config file surface.

**Tech Stack:** Rust 1.75, `serde`, `toml`, existing `anyhow`/`clap`/`reqwest`/`tokio` stack.

---

### Task 1: Add Anthropic Thinking Options

**Files:**
- Modify: `crates/hermes-providers/src/anthropic.rs`
- Test: `crates/hermes-providers/src/anthropic.rs`

- [ ] **Step 1: Write failing tests**

Add tests proving Anthropic no longer enables thinking from model-name guessing and only sends thinking when explicitly configured:

```rust
#[test]
fn thinking_defaults_to_off_for_claude_3_7() {
    let body = build_request_body("claude-3-7-sonnet-latest", &[], &[], true);
    let json = serde_json::to_value(body).unwrap();
    assert!(json.get("thinking").is_none());
    assert!(json.get("temperature").is_none());
}

#[test]
fn manual_thinking_is_explicit() {
    let body = build_request_body_with_options(
        "claude-3-7-sonnet-latest",
        &[],
        &[],
        true,
        AnthropicRequestOptions {
            thinking: Some(AnthropicThinking::Manual { budget_tokens: 8000 }),
        },
    );
    let json = serde_json::to_value(body).unwrap();
    assert_eq!(
        json["thinking"],
        serde_json::json!({ "type": "enabled", "budget_tokens": 8000 })
    );
    assert_eq!(json["temperature"], serde_json::json!(1.0));
}
```

- [ ] **Step 2: Run failing tests**

Run: `cargo test -p hermes-providers anthropic::tests::thinking_defaults_to_off_for_claude_3_7 anthropic::tests::manual_thinking_is_explicit`

Expected: fail because explicit request options do not exist and existing code auto-enables thinking for `3.7`.

- [ ] **Step 3: Implement explicit options**

Add:

```rust
#[derive(Debug, Clone, Default)]
pub struct AnthropicRequestOptions {
    pub thinking: Option<AnthropicThinking>,
}

#[derive(Debug, Clone)]
pub enum AnthropicThinking {
    Manual { budget_tokens: u32 },
    Adaptive { display: String },
}
```

Store `request_options` on `AnthropicProvider`, add `with_request_options()`, remove `supports_manual_thinking()`, and make `build_request_body()` delegate to `build_request_body_with_options(..., AnthropicRequestOptions::default())`.

- [ ] **Step 4: Run provider tests**

Run: `cargo test -p hermes-providers`

Expected: all provider tests pass.

### Task 2: Add Runtime Config Types

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/hermes-runtime/Cargo.toml`
- Create: `crates/hermes-runtime/src/config.rs`
- Modify: `crates/hermes-runtime/src/lib.rs`
- Test: `crates/hermes-runtime/src/config.rs`

- [ ] **Step 1: Write failing config tests**

Create tests for parsing:

```toml
[provider]
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
model = "mimo-v2.5"
base_url = "https://api.xiaomimimo.com/anthropic/v1"
api_key_header = "api-key"

[provider.thinking]
mode = "off"

[agent]
max_iterations = 12
disabled_toolsets = ["terminal"]
```

Expected Rust checks:

```rust
let config: HermesConfig = toml::from_str(input).unwrap();
assert_eq!(config.provider.kind, ProviderKind::Anthropic);
assert_eq!(config.agent.max_iterations, Some(12));
assert_eq!(config.provider.thinking.unwrap().mode, ThinkingMode::Off);
```

- [ ] **Step 2: Run failing tests**

Run: `cargo test -p hermes-runtime config`

Expected: fail because config module and `toml` dependency do not exist.

- [ ] **Step 3: Implement config module**

Add workspace dependency:

```toml
toml = "0.8"
```

Add runtime dependency:

```toml
toml.workspace = true
serde.workspace = true
anyhow.workspace = true
```

Create `config.rs` with `HermesConfig`, `ProviderConfig`, `ProviderKind`, `ThinkingConfig`, `ThinkingMode`, and `AgentConfig`. Include `HermesConfig::from_path(path: impl AsRef<Path>) -> anyhow::Result<Self>`.

- [ ] **Step 4: Run runtime tests**

Run: `cargo test -p hermes-runtime`

Expected: all runtime tests pass.

### Task 3: Build Agent From Config

**Files:**
- Modify: `crates/hermes-runtime/src/lib.rs`
- Test: `crates/hermes-runtime/src/config.rs`

- [ ] **Step 1: Write failing test for options merge**

Test that `HermesConfig::to_agent_options()` maps `agent.max_iterations` and `disabled_toolsets` into `AgentOptions`, preserving default system prompt and cwd when unspecified.

- [ ] **Step 2: Implement runtime construction**

Add:

```rust
impl AIAgent {
    pub fn from_config(config: HermesConfig, options_override: AgentOptions) -> anyhow::Result<Self> {
        ...
    }
}
```

Use `api_key_env` to read the API key. For `anthropic`, build `AnthropicProvider` with `with_api_key_header()` and explicit `AnthropicRequestOptions`. For `openai`, build `OpenAiProvider`. For `echo`, ignore provider credentials.

- [ ] **Step 3: Run runtime tests**

Run: `cargo test -p hermes-runtime`

Expected: all runtime tests pass.

### Task 4: Wire CLI `--config`

**Files:**
- Modify: `crates/hermes-cli/src/main.rs`
- Test: `cargo test --workspace`

- [ ] **Step 1: Add CLI argument**

Add:

```rust
#[arg(long)]
config: Option<PathBuf>,
```

- [ ] **Step 2: Load config in dispatch**

If `--config` is provided, load `HermesConfig::from_path`, overlay command-line `--model`, `--base-url`, `--max-iterations`, `--disabled-toolsets`, and `--cwd`, then call `AIAgent::from_config`.

- [ ] **Step 3: Preserve old env-driven path**

If `--config` is not provided, keep the existing `--provider` / env-var behavior unchanged.

- [ ] **Step 4: Run workspace tests**

Run: `cargo test --workspace`

Expected: all tests pass.

### Task 5: Update Docs

**Files:**
- Modify: `README.md`
- Create: `docs/superpowers/specs/2026-06-05-phase-9-config-and-skills-design.md`

- [ ] **Step 1: Document config example**

Add README example for MiMo:

```toml
[provider]
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
model = "mimo-v2.5"
base_url = "https://api.xiaomimimo.com/anthropic/v1"
api_key_header = "api-key"

[provider.thinking]
mode = "off"
```

- [ ] **Step 2: Document Skills follow-up**

Create a short spec section saying Skills will be loaded later from the same config surface:

```toml
[skills]
enabled = ["rust"]
paths = ["./skills"]
```

No runtime skill loading is implemented in this plan.

- [ ] **Step 3: Verify docs and tests**

Run: `cargo fmt --all --check` and `cargo test --workspace`.

Expected: both pass.
