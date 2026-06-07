# Perry Hermes

Perry Hermes (`perry_hermes`) is an AI agent runtime with streaming model
calls, tool use, skills, context compaction, and a terminal TUI. The codebase is
intentionally shaped so the CLI is only one adapter: the same runtime/session
model should work for a future gateway, Telegram bot, or other platform.

## Design

The central design rule is that a conversation is owned by a session, not by a
UI and not by the agent runtime.

```text
platform adapter
  e.g. Perry Hermes CLI, future gateway, Telegram
  owns: session_id -> AgentSession mapping and presentation

AIAgent
  shared runtime service
  owns: provider, tool registry, config, system-prompt composition, AgentLoop

AgentSession
  one conversation
  owns: SessionContext, message history, SessionState token facts

AgentLoop
  per-turn execution engine
  owns: no session lifetime

CompactionStrategy
  policy for rewriting messages
  owns: no history, no session state
```

This means a platform should create or look up an `AgentSession`, then call:

```rust
agent.run_session_turn(user_text, &session, cancel, on_event).await?;
agent.compact_session(&session, Some("optional focus")).await?;
```

The platform renders `LoopEvent`s. It should not keep a second copy of the
prompt history. In the current CLI, the TUI owns scrollback only; `AgentSession`
owns the actual model context.

## Crates

```text
perry-hermes-cli crate
  Perry Hermes CLI / ratatui TUI adapter

perry-hermes-agent crate
  runtime service, sessions, agent loop, tools, config, compaction

perry-hermes-core crate
  transport-free shared traits/types/errors

perry-hermes-providers crate
  OpenAI-compatible, Anthropic-compatible, and Echo providers

perry-hermes-skill-loader crate
  SKILL.md discovery, validation, and prompt rendering
```

`perry-hermes-core` has no IO concerns. Provider protocol details stay in
`perry-hermes-providers`. Product/platform behavior stays outside providers. Runtime
assembly lives in `perry-hermes-agent`.

## Context And Compaction

Context-window accounting uses provider-reported usage only. There is no
character/token estimate in the active logic.

The loop records the first real prompt-context token count in `SessionState`.
Automatic compaction runs only after a real provider response shows that the
configured context-window threshold has been reached.

The built-in compaction strategy is intentionally simple:

1. keep the system prompt, if present
2. keep the first user message
3. summarize every other message into one `[CONTEXT SUMMARY ...]` user message

After compaction, the best immediate usage signal is:

```text
first_prompt_context_tokens + summary_output_tokens
```

The next provider response becomes the source of truth again. Future changes to
the built-in compaction behavior should usually edit the summary prompt in
`crates/hermes-agent/src/compaction.rs`, not add more slicing rules.

## Configuration

Start from [examples/config/perry_hermes.toml](examples/config/perry_hermes.toml).

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
max_iterations = 10
disabled_toolsets = []
```

Provider credentials are read from the environment variable named by
`api_key_env`. Model names and `context_window_size` belong under
`[[providers.models]]`; the agent selects one with `[agent].default_provider`
and `[agent].default_model`.

For OpenAI-compatible services, change `base_url`. For Anthropic-compatible
services, set `kind = "anthropic"` and optionally `api_key_header`.

Useful agent options:

```toml
[agent]
disabled_toolsets = ["terminal"]
context_compression_enabled = true
context_compression_threshold_percent = 0.50
```

The config lookup order is:

1. `--config /path/to/perry_hermes.toml`
2. `~/.perry_hermes/config.toml`
3. `./perry_hermes.toml`

## CLI

```bash
cp examples/config/perry_hermes.toml perry_hermes.toml
cargo run -p perry-hermes-cli
```

The installed binary name is `perry-hermes`; the Cargo package is
`perry-hermes-cli`.

Run with a specific config or provider/model override:

```bash
cargo run -p perry-hermes-cli -- --config /path/to/perry_hermes.toml
cargo run -p perry-hermes-cli -- --provider minimax --model MiniMax-M3
```

Offline smoke config:

```bash
cat > perry_hermes.toml <<'TOML'
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

cargo run -p perry-hermes-cli
```

TUI controls:

- `/compact [focus]` compacts the current `AgentSession`
- `/clear` clears scrollback and resets the session
- `/quit` or `/exit` exits
- `Ctrl-C` cancels the current turn; a second `Ctrl-C` exits
- `Ctrl-D` exits

## Development

Common checks:

```bash
cargo fmt --all
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
cargo doc --no-deps
```

Targeted examples:

```bash
cargo run -p perry-hermes-providers --example live_smoke -- "say hi"
cargo run -p perry-hermes-agent --example live_tool_use -- "what time is it?"
cargo run -p perry-hermes-agent --example live_context_usage -- ~/.perry_hermes/config.toml
```

Testing guidance:

- use `ScriptedProvider` for deterministic multi-turn agent-loop tests
- keep live provider calls in examples or manual checks, not automated tests
- drive TUI behavior through `ratatui::backend::TestBackend`
- when changing session/context behavior, add tests at the `AgentSession` or
  `AIAgent` boundary rather than only in the CLI

## License

MIT
