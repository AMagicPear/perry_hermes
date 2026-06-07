# Hermes Rust

> A Rust reimplementation of Nous Research's [hermes-agent](https://github.com/NousResearch/hermes-agent): an AI agent runtime with tool use, streaming, skills, and an interactive CLI.

Current status: **Phases 0-10 are complete**. That includes the core agent loop, OpenAI-compatible and Anthropic-compatible providers, the built-in terminal tool, streaming output, Ctrl-C interruption, TOML-based runtime configuration, runtime skill loading, Phase 7 context compression, and Phase 10 (rename `hermes-skills` → `hermes-skill-loader`; replace the `hermes-cli` REPL with a `ratatui`-based TUI). See [the design doc](docs/superpowers/specs/2026-06-06-phase-10-rename-and-tui-design.md).

## Features

- **ReAct-style agent loop**: the model decides, calls tools, receives results, and continues until the task is complete.
- **Non-fatal tool failures**: tool errors are returned to the model instead of crashing the loop.
- **OpenAI-compatible provider support**: works with OpenAI and compatible endpoints such as DeepSeek, MiniMax, Ollama, and vLLM via configurable `base_url`.
- **Anthropic-compatible provider support**: supports the Anthropic Messages API and compatible services that require custom API key headers.
- **TOML runtime configuration**: the CLI resolves config files in this order: `--config`, `~/.perry_hermes/config.toml`, then `./hermes.toml`.
- **Cooperative cancellation**: a shared `CancellationToken` flows through model calls and tool execution, enabling graceful Ctrl-C interruption.
- **Interactive TUI** (replacing the original REPL): full-screen `ratatui` interface with multi-turn chat, streaming output, live tool rendering, slash commands, per-agent toolset filtering, and a dynamic-height input box that grows with wrapped text.
- **Built-in context compression**: compression is enabled by default, can be triggered manually with `/compact [focus]`, and reports completed, skipped, and failed compactions in the TUI status line.
- **Runtime skill loading**: `SKILL.md` files under `~/.perry_hermes/skills/` are discovered and injected into the system prompt.
- **Robust terminal tooling**: concurrent stdout/stderr draining avoids pipe deadlocks, and output truncation is aligned with Python Hermes behavior.
- **Clear crate boundaries**: `hermes-core` stays transport-agnostic, while runtime orchestration lives in `hermes-agent` and the product shell lives in `hermes-cli` (TUI). Phase 11 will add `hermes-gateway` as a peer adapter of `hermes-agent` for Slack/Discord/Telegram.

## Architecture

```text
hermes-cli (ratatui TUI)
  └─ hermes-agent
       ├─ hermes-core
       ├─ hermes-providers
       └─ hermes-skill-loader
```

> Note: `hermes-skills` was renamed to `hermes-skill-loader` in Phase 10 to make its data-only scope explicit (see [the design doc](docs/superpowers/specs/2026-06-06-phase-10-rename-and-tui-design.md)). Future MCP support will live in its own `hermes-mcp-*` crate, not as a "utilities" catch-all.

### Crates

| Crate | Responsibility | Key concepts |
|---|---|---|
| `hermes-core` | Shared types, traits, and errors with no IO concerns | `Provider`, `Tool`, `Message`, `Completion`, `FinishReason` |
| `hermes-providers` | Provider implementations and streaming protocol adapters | `OpenAiProvider`, `AnthropicProvider`, `EchoProvider` |
| `hermes-agent` | Runtime assembly, loop execution, session context, and built-in tools | `AIAgent`, `AgentLoop`, `SessionContext`, built-in tool registry |
| `hermes-skill-loader` | Skill data loading, validation, and prompt-ready metadata | `SKILL.md` frontmatter/layout/validate, `render_system_prompt_block` |
| `hermes-cli` | Interactive TUI entrypoint (ratatui) | config resolution, TUI event loop, event rendering |

### Runtime boundaries

- **`Provider`** is the async model interface used by the agent loop.
- **`Tool`** is the async execution interface for built-in and future external tools.
- **Tool registry and toolset filtering** are runtime concerns handled by `hermes-agent`.
- **Prompt and session composition** are isolated from the CLI so the runtime can stay reusable.
- **Display logic** is kept in the CLI layer instead of leaking into the runtime.

## Quick Start

### Requirements

- Rust 1.75+
- `direnv` (optional)

### Configuration

Start from the sample config in [examples/config/hermes.toml](/Users/amagicpear/projects/perry_hermes/examples/config/hermes.toml).

```toml
[[providers]]
name = "openai-main"
kind = "openai" # openai | anthropic | echo
api_key_env = "OPENAI_API_KEY"
base_url = "https://api.openai.com/v1"

[[providers.models]]
name = "gpt-4.1-mini"
context_window_size = 1_047_576

[agent]
default_provider = "openai-main"
default_model = "gpt-4.1-mini"
```

Provider credentials are read from the environment variable named by `api_key_env`. `providers.name` is the display name shown in the TUI. Model names and their `context_window_size` belong under `[[providers.models]]`; the agent selects one with `[agent].default_provider` and `[agent].default_model`.

For OpenAI-compatible services such as DeepSeek, MiniMax, Ollama, or vLLM, change `base_url` and add the models you want to use.

For Anthropic-compatible services, you can also set `api_key_header` when the endpoint expects something other than the default header.

Context compression is enabled by default. To disable it explicitly:

```toml
[agent]
context_compression_enabled = false
```

### Build

```bash
cargo build
```

### Run the CLI

Copy the sample config and edit it for your provider:

```bash
cp examples/config/hermes.toml hermes.toml
cargo run -p hermes-cli
```

The CLI resolves config files in this order:

1. `--config /path/to/file.toml`
2. `~/.perry_hermes/config.toml`
3. `./hermes.toml`

Run with an explicit working directory:

```bash
cargo run -p hermes-cli -- --cwd /tmp
```

Run with an explicit config file:

```bash
cargo run -p hermes-cli -- --config /path/to/hermes.toml
```

Override the configured provider/model for one run:

```bash
cargo run -p hermes-cli -- --provider minimax --model MiniMax-M2.7
```

Offline smoke test with the `echo` provider:

```bash
cat > hermes.toml <<'TOML'
[[providers]]
name = "local"
kind = "echo"

[[providers.models]]
name = "echo"
context_window_size = 128_000

[agent]
default_provider = "local"
default_model = "echo"
TOML
cargo run -p hermes-cli
```

Disable the terminal toolset:

```toml
[agent]
disabled_toolsets = ["terminal"]
```

Increase the agent iteration budget:

```toml
[agent]
max_iterations = 50
```

Example full config:

```toml
[[providers]]
name = "minimax"
kind = "anthropic"
api_key_env = "MINIMAX_API_KEY"
base_url = "https://api.xiaomimimo.com/anthropic/v1"
api_key_header = "api-key"

[[providers.models]]
name = "MiniMax-M3"
context_window_size = 1_000_000

[[providers.models]]
name = "MiniMax-M2.7"
context_window_size = 204_800

[providers.thinking]
mode = "off" # off | manual | adaptive
# manual: budget_tokens = 8000
# adaptive: display = "summarized", effort = "medium"

[agent]
default_provider = "minimax"
default_model = "MiniMax-M3"
max_iterations = 10
disabled_toolsets = []
# context_compression_enabled = false
# context_compression_threshold_percent = 0.50
```

### TUI controls

- Type any message to send it to the agent.
- `/quit` or `/exit` exits the TUI.
- `/compact` runs a manual context compaction pass.
- `/compact <focus>` runs a manual compaction pass while prioritizing that topic in the generated summary.
- `Ctrl-C` once cancels the current turn.
- `Ctrl-C` twice exits the TUI.
- `Ctrl-D` exits the TUI.

### Examples

Provider-only smoke example:

```bash
cargo run -p hermes-providers --example live_smoke -- "say hi"
```

Single-turn agent example with tools:

```bash
cargo run -p hermes-agent --example live_tool_use -- "what time is it?"
```

## Development

### Common commands

```bash
cargo build
cargo test
cargo test -p hermes-core
cargo test -p hermes-agent --test tool_dispatch
cargo clippy --workspace --all-targets -- -D warnings
```

### CLI smoke check

```bash
cat > /tmp/hermes-smoke.toml <<'TOML'
[[providers]]
name = "local"
kind = "echo"

[[providers.models]]
name = "echo"
context_window_size = 128_000

[agent]
default_provider = "local"
default_model = "echo"
TOML
printf 'hello\n' | cargo run -p hermes-cli --quiet -- --config /tmp/hermes-smoke.toml
```

### Test layout

Current integration tests focus on core behavior and module boundaries rather than high-level scripted workflows.

`crates/hermes-agent/tests/`

- `arg_validation.rs`
- `bash.rs`
- `files.rs`
- `skills.rs`
- `skills_injection.rs`
- `tool_dispatch.rs`
- `usage_metrics.rs`
- `context_compression.rs`

`crates/hermes-providers/tests/`

- `openai.rs`
- `openai_stream.rs`
- `anthropic.rs`
- `tool_call_roundtrip.rs`

## Documentation

| File | Purpose |
|---|---|
| [docs/history/rust-port-design.md](/Users/amagicpear/projects/perry_hermes/docs/history/rust-port-design.md) | Historical design draft for the Rust port |
| [docs/history/hermes-comparison.md](/Users/amagicpear/projects/perry_hermes/docs/history/hermes-comparison.md) | Comparison notes between the Rust implementation and Python Hermes |
| [docs/superpowers/specs/2026-06-06-builtin-tools-expansion-design.md](/Users/amagicpear/projects/perry_hermes/docs/superpowers/specs/2026-06-06-builtin-tools-expansion-design.md) | Built-in tools expansion design |
| [docs/superpowers/specs/2026-06-06-architecture-cohesion-refactor-design.md](/Users/amagicpear/projects/perry_hermes/docs/superpowers/specs/2026-06-06-architecture-cohesion-refactor-design.md) | Architecture cleanup and cohesion refactor |
| [docs/superpowers/specs/2026-06-06-phase-10-rename-and-tui-design.md](/Users/amagicpear/projects/perry_hermes/docs/superpowers/specs/2026-06-06-phase-10-rename-and-tui-design.md) | Phase 10: rename `hermes-skills` → `hermes-skill-loader` + replace `hermes-cli` REPL with `ratatui` TUI |
| [AGENTS.md](/Users/amagicpear/projects/perry_hermes/AGENTS.md) | Development guidance for in-repo agent workflows |

## Roadmap

| Phase | Goal | Status |
|---|---|---|
| Phase 0 | Workspace skeleton and core traits | Done |
| Phase 1 | Echo loop with a minimal provider | Done |
| Phase 2 | OpenAI provider with real model calls | Done |
| Phase 3 | Terminal tool execution | Done |
| Phase 4 | Interactive CLI and REPL | Done |
| Phase 5 | Streaming output | Done |
| Phase 6 | Interrupt handling with Ctrl-C | Done |
| Phase 7 | Context compression | Done |
| Phase 8 | Anthropic provider | Done |
| Phase 9 | Skill loading and prompt injection | Done |
| Phase 10 | TUI with `ratatui` (replace `hermes-cli` REPL; rename `hermes-skills` → `hermes-skill-loader`) | Done |
| Phase 11 | Platform gateway integrations | Pending |
| Phase 12 | Curator and learning loop | Pending |

## Known Limitations

- `ToolContext.permissions` is modeled but not enforced yet.
- Unknown provider `finish_reason` values still collapse into a generic runtime error path.
- OpenAI-compatible providers support text and `image_url` content parts, but not every possible multimodal part type yet.
- The OpenAI-compatible provider can normalize `<think>...</think>` fallback content into reasoning text when a provider does not expose a native reasoning field.
- Anthropic-compatible thinking is off by default and must be enabled explicitly in TOML.
- Reusing the same terminal tool instance under concurrent cancellation has not been stress-tested deeply.

## License

MIT
