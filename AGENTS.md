# AGENTS.md

This file is for coding agents working in this repository. Keep it current with
the code, not with historical phase plans.

## Project Intent

Perry Hermes (`perry_hermes`) is moving toward a platform-neutral agent
runtime. The CLI is only one adapter. Future gateway/Telegram-style
integrations should share the same runtime and session model instead of
reimplementing context management.

This project is experimental and has no compatibility promise. When a cleaner
architecture conflicts with older API shape, prefer the cleaner architecture
unless the current task explicitly asks for compatibility.

## Design Principles

- A conversation is an `AgentSession`.
- `AIAgent` is a reusable runtime service, not a session.
- `AgentLoop` runs turns; it does not own session lifetime.
- Platform adapters own presentation and `session_id -> AgentSession` mapping.
- `AgentSession` owns message history and token facts.
- Context compaction is session behavior driven by provider-reported usage.
- The TUI owns scrollback only; it must not own prompt history.
- Provider protocol details stay in `perry-hermes-providers`.
- Shared traits/data stay in `perry-hermes-core`, without IO concerns.

The important call shape is:

```rust
agent.run_session_turn(user_text, &session, cancel, on_event).await?;
agent.compact_session(&session, focus).await?;
```

Avoid adding public APIs where callers pass arbitrary `Vec<Message>` as their
own conversation store. That recreates split-brain context ownership.

## Architecture

```text
perry-hermes-cli crate
  Perry Hermes CLI / ratatui platform adapter

perry-hermes-agent crate
  AIAgent
  AgentSession
  AgentLoop
  SummaryCompactor
  built-in tools

perry-hermes-core crate
  Provider / Tool / Message / Usage / errors

perry-hermes-providers crate
  OpenAI-compatible / Anthropic-compatible / Echo

perry-hermes-skill-loader crate
  SKILL.md loading and prompt block rendering
```

Key files:

| File | Purpose |
|---|---|
| `crates/hermes-agent/src/runtime_agent.rs` | `AIAgent` construction and session-facing APIs |
| `crates/hermes-agent/src/session.rs` | `AgentSession`, `SessionContext`, `SessionState`, message history |
| `crates/hermes-agent/src/loop_engine/` | turn execution, provider streaming, tool dispatch, automatic compaction trigger |
| `crates/hermes-agent/src/compaction.rs` | built-in summary compaction strategy and prompt |
| `crates/hermes-agent/src/config.rs` | TOML config model and provider/model resolution |
| `crates/hermes-cli/src/tui/` | presentation state, input handling, event rendering |
| `crates/hermes-providers/src/` | provider protocol adapters |

## Session Model

`AgentSession` is the unit that should map to a human-visible conversation:

- CLI process: one session
- Telegram: usually one session per chat/thread
- gateway: a store keyed by platform/session id

`AgentSession` owns:

- `SessionContext`: `session_id`, `working_dir`
- message history
- `SessionState`: provider-derived token facts

`AIAgent::run_session_turn` appends the user message to the session, runs the
current session history through `AgentLoop`, then writes the returned history
back to the session. Failed turns that include preserved history also update
the session.

`AIAgent::compact_session` reads the session history, compacts it, and writes
the compacted history back only when compaction succeeds.

## Context Compaction

Do not estimate context usage from characters. Token accounting comes from
provider usage.

Automatic compaction happens only after a provider response reports context
usage at or above the configured threshold:

```text
context_compression_threshold_percent * selected_model.context_window_size
```

The current built-in strategy keeps:

1. system prompt, if present
2. first user message
3. one LLM-generated summary message for everything else

The immediate post-compaction usage signal is:

```text
SessionState.first_prompt_context_tokens + summary_completion.output_tokens
```

The next provider response is the source of truth again. If the compaction
mechanism needs to change, prefer editing `build_summary_prompt` in
`crates/hermes-agent/src/compaction.rs` before adding new retention logic.

## Config Rules

Current config shape:

```toml
[[providers]]
name = "local"
kind = "echo"

[[providers.models]]
name = "echo"
context_window_size = 128_000

[agent]
default_provider = "local"
default_model = "echo"
```

Rules:

- keep `[[providers]]` plus `[[providers.models]]`
- do not reintroduce old single `[provider]`
- `context_window_size` belongs on each model
- selected provider/model comes from `[agent].default_provider` and
  `[agent].default_model`
- CLI may override provider/model for one run with `--provider` / `--model`
- `disabled_toolsets` is configured at startup
- live provider tests should stay manual or in examples

## Testing

Run before claiming completion:

```bash
cargo fmt --all
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
cargo doc --no-deps
```

Use focused commands while iterating:

```bash
cargo test -p perry-hermes-agent --test context_compression
cargo test -p perry-hermes-agent --test tool_dispatch
cargo test -p perry-hermes-cli tui
```

Testing patterns:

- `ScriptedProvider` is the default for deterministic loop tests.
- Provider HTTP tests may use `httpmock` or a raw `tokio::net::TcpListener`
  when request body inspection is needed.
- TUI tests should drive `App` and `LoopEvent`s; do not require a real terminal.
- When tests mutate process-wide env vars, serialize them with a mutex.
- Do not add live provider calls to automated tests.

## Common Commands

```bash
cargo build
cargo run -p perry-hermes-cli
cargo run -p perry-hermes-cli -- --config /path/to/perry_hermes.toml
cargo run -p perry-hermes-agent --example live_tool_use -- "what time is it?"
cargo run -p perry-hermes-agent --example live_context_usage -- ~/.perry_hermes/config.toml
```

Offline CLI smoke:

```bash
cat > /tmp/perry-hermes-smoke.toml <<'TOML'
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

echo "hello" | cargo run -p perry-hermes-cli --quiet -- --config /tmp/perry-hermes-smoke.toml
```
