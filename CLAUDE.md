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

# CLI smoke (offline, no API key needed)
echo "hello" | cargo run -p hermes-cli --quiet -- --provider echo
```

direnv auto-loads `.envrc` on `cd` — no manual env var exports needed once set up.

## Architecture

Dependency direction is strictly downward — no reverse edges between crates.

```
hermes-cli (interactive REPL — Phase 4)
  └─ hermes-runtime (product API facade — AIAgent)
       └─ hermes-loop (agent loop state machine)
            ├─ hermes-core (types, traits, errors — no IO)
            ├─ hermes-providers (OpenAI adapter, echo mock)
            └─ hermes-tools (BashTool)
```

`hermes-providers` and `hermes-tools` are independent — adding a tool never touches providers, and vice versa.

**Core boundaries**:

- `Provider` — async `stream(messages, tools, cancel) -> CompletionStream`. `ProviderError` via `?` propagates cleanly into `LoopError`.
- `Tool` — async `execute(args, ctx, cancel) -> ToolOutput`. `ToolError` is **non-fatal**: the loop wraps it in `role: tool` content so the LLM sees the error and can pivot.
- `InMemoryRegistry` — maps tool names to `Arc<dyn Tool>`. Runtime builds it after applying toolset policy.
- `LoopEvent` — typed presentation events for CLI/gateway. The agent reports what happened; adapters decide how to render it.

**The agent loop** (`hermes-loop/src/agent.rs`): `AgentLoop` holds `Arc<dyn Provider>` plus an `InMemoryRegistry`. The `run()` method is a state machine loop — on `FinishReason::ToolUse` it dispatches tools sequentially, appends `role: tool` messages, and calls the provider again. `on_event` callback is the only side-channel (CLI uses it for spinner/activity feed; tests collect event strings). **`run()` takes a `ToolContext`** so runtime/gateway controls `session_id`, `working_dir`, and permissions.

**OpenAI adapter** (`hermes-providers/src/openai.rs`): `OpenAiProvider::with_base_url()` lets it talk to any OpenAI-compatible endpoint (DeepSeek, MiniMax, Ollama, vLLM). The `tool_calls` field round-trips: assistant's `tool_calls` are serialized back into the next request body so the LLM remembers which tools it called. OpenAI sends `arguments` as a JSON **string** (not object) — must `serde_json::from_str` on parse and `to_string` on serialize. `tool_choice: "auto"` is **omitted** when the tool list is empty (some providers reject it). `finish_reason` is parsed via `FinishReason::from_provider_str` (renamed from `from_str` to avoid colliding with the std `FromStr` trait).

**Runtime + CLI** (`crates/hermes-runtime/src/lib.rs`, `crates/hermes-cli/src/main.rs`): runtime is the shared composition point for CLI and future gateway. `AIAgent::from_config(HermesConfig)` and `AIAgent::new(provider, HermesConfig)` are the two constructors; runtime builds the registry, loop, and resolves the `Provider` from the config. Per-run `working_dir` / `session_id` travel in a `SessionContext` passed to `run_turn` / `run_messages` (the runtime is reusable across sessions). CLI only parses args, resolves the config path (`--config` → `~/.perry_hermes/config.toml` → `./hermes.toml`, error if none), maintains REPL history, and renders `LoopEvent`s. Multi-turn: `run_result.messages` becomes the next turn's `history`. Ctrl-C: first cancels the current turn via `CancellationToken`, second exits the loop. Slash commands: `/quit`, `/exit`. Provider-specific things (`OPENAI_API_KEY` etc.) live in `[provider]` of the TOML file; the runtime never reads the environment for defaults.

## TDD Workflow

Strict RED→GREEN→REFACTOR. No production code without a failing test first.

- **RED**: write test, `cargo test`, confirm it fails for the right reason
- **GREEN**: minimal code to pass, `cargo test` again, all green
- **REFACTOR**: clean up, tests still green

For loop tests, `ScriptedProvider` (in `crates/hermes-loop/tests/`) returns a fixed sequence of `Completion`s — use it for multi-iteration scenarios. For OpenAI provider HTTP tests, `httpmock` mocks the server. For request body inspection, use a raw `tokio::net::TcpListener` (httpmock 0.7 doesn't expose captured bodies).

## Design Doc

Current code is authoritative. `plans/rust-port-design.md` is a historical design draft — useful for intent and roadmap, but some API sketches are stale after the runtime/streaming simplification. `plans/hermes-comparison.md` tracks divergence from the Python source and has a current-status note at the top.

## Known Issues (from comparison report)

**Resolved since phase 4:**

- ✅ BashTool pipe deadlock — `crates/hermes-tools/src/bash.rs` now drains `stdout` and `stderr` concurrently via `tokio::join!` instead of sequentially.
- ✅ CLI not wired to runtime — `hermes-cli` binary now uses `AIAgent`'s pieces directly; REPL is end-to-end runnable.
- ✅ `tool_choice: "auto"` sent on empty tool list — OpenAI provider now sets `tool_choice: None` when `tools.is_empty()`.
- ✅ No streaming (Phase 5) — `Provider::stream()` is the only required method; `OpenAiProvider` parses SSE and yields `CompletionDelta`; `AgentLoop::run` drives the stream via `tokio::select!` and emits `ContentDelta` / `ReasoningDelta` / `ToolCallPartial` events. CLI prints tokens as they arrive. Cancel mid-stream preserves partial content via `LoopError::CancelledWith(Message)`.
- ✅ Runtime facade too thin — CLI now goes through `hermes-runtime::AIAgent`; runtime is the shared composition point for future gateway.
- ✅ Over-abstracted registry/loop — removed `ToolRegistry` trait and `AgentLoop<P, R>` generics; current loop uses `Arc<dyn Provider>` + `InMemoryRegistry`.

**Still open (before phase 7):**

- Permission model is still coarse — `BashTool` checks `subprocess`, but runtime currently always enables it and no finer gateway policy exists yet.
- Unknown `finish_reason` maps to `FinishReason::Error`, but provider diagnostics are still coarse.
- OpenAI-compatible providers now serialize `Content::Parts` as text/image_url content arrays; other media part types are not modeled yet.
- `BashTool` does not kill concurrent children — under heavy parallel load the `child.kill().await` path is untested.

**P1 (next up):**

- ~~No streaming yet (Phase 5)~~ — resolved.
- No `IterationBudget` (refund / grace call / subagent budget) — `LoopConfig.max_iterations` is a flat `u32`. (Phase 7)
- `Toolset` filtering works at registry construction time (via `--disabled-toolsets`) but is not reactive to per-turn changes.
