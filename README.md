# Perry Hermes

Perry Hermes (`perry_hermes`) is an AI agent runtime with streaming model
calls, tool use, skills, context compaction, and a terminal TUI. It is inspired
by Nous Research's [Hermes Agent][hermes-agent] and keeps one explicit long-term
goal from that project: reproduce the self-learning mechanism while keeping
Perry Hermes' own runtime, session model, and platform adapters cleanly
separated.

## Features

- **ReAct-style agent loop**: the model can reason, call tools, receive tool
  results, and continue until the turn is complete.
- **Session-owned conversation state**: `AgentSession` owns model history,
  context-window facts, and compaction state; platform adapters only own
  presentation and session lookup.
- **Provider-reported context accounting**: context usage comes from provider
  usage data, not character/token estimates.
- **Simple context compaction**: manual or threshold-triggered compaction keeps
  the system prompt, first user message, and one LLM-generated summary.
- **OpenAI-compatible and Anthropic-compatible providers**: provider adapters
  live below the agent runtime and share the transport-free core contracts.
- **Runtime skills**: `SKILL.md` files are loaded into the system prompt, and
  built-in skill tools let the model inspect available local skills.
- **Terminal and file tools**: built-in tools expose shell execution, file
  reads/writes, and skill discovery through one registry.
- **Ratatui TUI**: the CLI is an adapter around the shared runtime/session
  model, with slash commands, streaming output, cancellation, and compact status
  events.
- **Multi-platform gateway**: `hermes-gateway` dispatches conversations across
  Telegram (via [teloxide](https://github.com/teloxide/teloxide)) and QQ/Guild
  (via [qq-bot-rs](https://github.com/yenharvey/qq-bot-rs)), sharing the same
  `AgentSession` model as the CLI.
- **Self-learning target**: the roadmap points toward Hermes Agent-style
  learning from experience, skill generation, and skill refinement; comparison
  notes live in [docs/history/hermes-comparison.md][hermes-comparison].

## Architecture

The central design rule is that a conversation is owned by a session, not by the
UI and not by the agent runtime.

```text
platform adapter
  e.g. Perry Hermes CLI, hermes-gateway (Telegram, QQ/Guild)
  owns: session_id -> AgentSession mapping and presentation

AgentLoop
  shared runtime service + per-turn execution engine
  owns: provider, tool registry, config, system-prompt composition, loop execution

AgentSession
  one conversation
  owns: SessionContext, message history, SessionState token facts

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

```text
crates/hermes-gateway
  package: perry-hermes-gateway
  owns: platform adapters (Telegram, QQ/Guild), session dispatch, gateway runner

crates/hermes-cli
  package: perry-hermes-cli
  binary: perry-hermes
  owns: TUI, config lookup, platform presentation

crates/hermes-agent
  package: perry-hermes-agent
  owns: AgentLoop, AgentSession, config, compaction, tool catalog

crates/hermes-core
  package: perry-hermes-core
  owns: transport-free Provider / Tool / Message / Usage / errors

crates/hermes-providers
  package: perry-hermes-providers
  owns: OpenAI-compatible / Anthropic-compatible / Echo providers

crates/hermes-skill-tools
  package: perry-hermes-skill-tools
  owns: SKILL.md discovery, validation, prompt rendering, and all seven built-in LLM tools
```

| Layer | Key files | Boundary |
|---|---|---|
| CLI adapter | [crates/hermes-cli/src/main.rs][cli-main], [crates/hermes-cli/src/tui/][tui] | Owns presentation and creates/uses an `AgentSession`; does not own prompt history. |
| Gateway | [crates/hermes-gateway/src/][gateway] | Dispatches `AgentSession` across Telegram and QQ/Guild platform adapters; shares the same runtime as the CLI. |
| Agent runtime | [crates/hermes-agent/src/loop_engine/][loop-engine], [crates/hermes-agent/src/session.rs][session] | Owns runtime assembly, loop engine, and session APIs shared by CLI and gateway. |
| Compaction | [crates/hermes-agent/src/compaction.rs][compaction] | Encodes the summary prompt and the current "anchors plus one summary" policy. |
| Core contracts | [crates/hermes-core/src/][core] | Defines shared traits/types without provider, CLI, or filesystem policy. |
| Providers | [crates/hermes-providers/src/][providers] | Translates external provider protocols into core streaming types. |
| Skills & tools | [crates/hermes-skill-tools/src/][skill-tools] | Loads and validates `SKILL.md`, renders the prompt block, and provides all seven built-in tools. |

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
  `AgentLoop` boundary rather than only in the CLI

## Acknowledgments

Perry Hermes builds on excellent open-source work from the Rust ecosystem:

- **[teloxide](https://github.com/teloxide/teloxide)** — the Telegram bot framework that powers the Telegram platform adapter in `hermes-gateway`.
- **[qq-bot-rs](https://github.com/yenharvey/qq-bot-rs)** — the QQ bot SDK that enables QQ/Guild integration in `hermes-gateway`.
- **[ratatui](https://github.com/ratatui/ratatui)** — the terminal UI library behind the Perry Hermes CLI.
- **[Hermes Agent](https://github.com/NousResearch/hermes-agent)** by Nous Research — the original project that inspired the self-learning architecture and agent design.

## License

MIT

[hermes-agent]: https://github.com/NousResearch/hermes-agent
[hermes-comparison]: docs/history/hermes-comparison.md
[cli-main]: crates/hermes-cli/src/main.rs
[tui]: crates/hermes-cli/src/tui/
[session]: crates/hermes-agent/src/session.rs
[loop-engine]: crates/hermes-agent/src/loop_engine/
[compaction]: crates/hermes-agent/src/compaction.rs
[core]: crates/hermes-core/src/
[providers]: crates/hermes-providers/src/
[skill-tools]: crates/hermes-skill-tools/src/
[gateway]: crates/hermes-gateway/src/
