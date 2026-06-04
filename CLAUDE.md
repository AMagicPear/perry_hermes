# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test

```bash
cargo build                              # all workspace crates
cargo test                               # all unit + integration tests
cargo test -p hermes-core                # single crate
cargo test -p hermes-loop --test tool_dispatch  # single integration test
cargo clippy --all-targets --all-features -- -D warnings

# Live smoke (needs OPENAI_API_KEY, OPENAI_BASE_URL, OPENAI_MODEL in env)
cargo run -p hermes-providers --example live_smoke
cargo run -p hermes-runtime --example live_tool_use -- "what time is it?"
```

direnv auto-loads `.envrc` on `cd` — no manual env var exports needed once set up.

## Architecture

Dependency direction is strictly downward — no reverse edges between crates.

```
hermes-cli (binary stub, phase 0)
  └─ hermes-runtime (product API facade — AIAgent)
       └─ hermes-loop (agent loop state machine)
            ├─ hermes-core (types, traits, errors — no IO)
            ├─ hermes-providers (OpenAI adapter, echo mock)
            └─ hermes-tools (BashTool)
```

`hermes-providers` and `hermes-tools` are independent — adding a tool never touches providers, and vice versa.

**Three core traits** (all in `hermes-core`):

- `Provider` — async `complete(messages, tools, cancel) -> Completion`. `ProviderError` via `?` propagates cleanly into `LoopError`.
- `Tool` — async `execute(args, ctx, cancel) -> ToolOutput`. `ToolError` is **non-fatal**: the loop wraps it in `role: tool` content so the LLM sees the error and can pivot.
- `ToolRegistry` — maps tool names to `Arc<dyn Tool>`. `InMemoryRegistry` is HashMap-backed with builder-style `.register()`.

**The agent loop** (`hermes-loop/src/agent.rs`): `AgentLoop<P, R>` is generic over Provider and ToolRegistry. The `run()` method is a state machine loop — on `FinishReason::ToolUse` it dispatches tools sequentially, appends `role: tool` messages, and calls the provider again. `on_event` callback is the only side-channel (CLI uses it for spinner/activity feed; tests collect event strings).

**OpenAI adapter** (`hermes-providers/src/openai.rs`): `OpenAiProvider::with_base_url()` lets it talk to any OpenAI-compatible endpoint (DeepSeek, MiniMax, Ollama, vLLM). The `tool_calls` field round-trips: assistant's `tool_calls` are serialized back into the next request body so the LLM remembers which tools it called. OpenAI sends `arguments` as a JSON **string** (not object) — must `serde_json::from_str` on parse and `to_string` on serialize.

## TDD Workflow

Strict RED→GREEN→REFACTOR. No production code without a failing test first.

- **RED**: write test, `cargo test`, confirm it fails for the right reason
- **GREEN**: minimal code to pass, `cargo test` again, all green
- **REFACTOR**: clean up, tests still green

For loop tests, `ScriptedProvider` (in `crates/hermes-loop/tests/`) returns a fixed sequence of `Completion`s — use it for multi-iteration scenarios. For OpenAI provider HTTP tests, `httpmock` mocks the server. For request body inspection, use a raw `tokio::net::TcpListener` (httpmock 0.7 doesn't expose captured bodies).

## Design Doc

`plans/rust-port-design.md` is the master reference — types, traits, loop code, crate layout, 12-phase roadmap, Rust-specific pitfalls. `plans/hermes-comparison.md` tracks divergence from the Python source. Both are authoritative for architectural decisions.

## Known Issues (from comparison report)

**Before phase 4**: BashTool pipe deadlock (large stdout blocks), `ToolContext.permissions` not enforced, CLI not wired to runtime.

**P1**: Toolset filtering not wired into schema/dispatch, unknown `finish_reason` defaults to Stop, `Content::Parts` silently dropped, empty tool list still sends `tool_choice: "auto"`.
