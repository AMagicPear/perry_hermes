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

**Three core traits** (all in `hermes-core`):

- `Provider` — async `complete(messages, tools, cancel) -> Completion`. `ProviderError` via `?` propagates cleanly into `LoopError`.
- `Tool` — async `execute(args, ctx, cancel) -> ToolOutput`. `ToolError` is **non-fatal**: the loop wraps it in `role: tool` content so the LLM sees the error and can pivot.
- `ToolRegistry` — maps tool names to `Arc<dyn Tool>`. `InMemoryRegistry` is HashMap-backed with builder-style `.register()`. Supports `toolsets()` / `tools_in_toolset()` for filtering.

**The agent loop** (`hermes-loop/src/agent.rs`): `AgentLoop<P, R>` is generic over Provider and ToolRegistry. The `run()` method is a state machine loop — on `FinishReason::ToolUse` it dispatches tools sequentially, appends `role: tool` messages, and calls the provider again. `on_event` callback is the only side-channel (CLI uses it for spinner/activity feed; tests collect event strings). **`run()` takes a `ToolContext`** so the caller (CLI / runtime) controls `session_id`, `working_dir`, and `permissions` — the loop no longer hardcodes `"default"` and `std::env::current_dir()`.

**OpenAI adapter** (`hermes-providers/src/openai.rs`): `OpenAiProvider::with_base_url()` lets it talk to any OpenAI-compatible endpoint (DeepSeek, MiniMax, Ollama, vLLM). The `tool_calls` field round-trips: assistant's `tool_calls` are serialized back into the next request body so the LLM remembers which tools it called. OpenAI sends `arguments` as a JSON **string** (not object) — must `serde_json::from_str` on parse and `to_string` on serialize. `tool_choice: "auto"` is **omitted** when the tool list is empty (some providers reject it). `finish_reason` is parsed via `FinishReason::from_provider_str` (renamed from `from_str` to avoid colliding with the std `FromStr` trait).

**CLI** (`crates/hermes-cli/src/main.rs`): clap-derived args, tokio runtime, `dispatch()` branches on `--provider openai|echo` and instantiates `AgentLoop` with the concrete provider type (avoids `Arc<dyn Provider>` which doesn't satisfy `P: Provider`). The REPL is a generic `run_repl<P, R>` over any `AgentLoop<P, R>`. Events render to stderr as `… ` / `📦 tool(args)` / `← ⚡ result` lines with truncated previews. Multi-turn: `run_result.messages` becomes the next turn's `history`. Ctrl-C: first cancels the current turn via `CancellationToken`, second exits the loop. Slash commands: `/quit`, `/exit`. `--disabled-toolsets core|terminal` filters the registry at construction time.

## TDD Workflow

Strict RED→GREEN→REFACTOR. No production code without a failing test first.

- **RED**: write test, `cargo test`, confirm it fails for the right reason
- **GREEN**: minimal code to pass, `cargo test` again, all green
- **REFACTOR**: clean up, tests still green

For loop tests, `ScriptedProvider` (in `crates/hermes-loop/tests/`) returns a fixed sequence of `Completion`s — use it for multi-iteration scenarios. For OpenAI provider HTTP tests, `httpmock` mocks the server. For request body inspection, use a raw `tokio::net::TcpListener` (httpmock 0.7 doesn't expose captured bodies).

## Design Doc

`plans/rust-port-design.md` is the master reference — types, traits, loop code, crate layout, 12-phase roadmap, Rust-specific pitfalls. `plans/hermes-comparison.md` tracks divergence from the Python source. Both are authoritative for architectural decisions.

## Known Issues (from comparison report)

**Resolved since phase 4:**

- ✅ BashTool pipe deadlock — `crates/hermes-tools/src/bash.rs` now drains `stdout` and `stderr` concurrently via `tokio::join!` instead of sequentially.
- ✅ CLI not wired to runtime — `hermes-cli` binary now uses `AIAgent`'s pieces directly; REPL is end-to-end runnable.
- ✅ `tool_choice: "auto"` sent on empty tool list — OpenAI provider now sets `tool_choice: None` when `tools.is_empty()`.

**Still open (before phase 5):**

- `ToolContext.permissions` not enforced — the field exists, no tool consults it.
- Unknown `finish_reason` defaults to `Stop` — `FinishReason::from_provider_str` silently maps anything unrecognized to `Stop` instead of returning `Error`.
- `Content::Parts` silently dropped — multimodal content round-trips as `<multimodal content>` in the CLI.
- `BashTool` does not kill concurrent children — under heavy parallel load the `child.kill().await` path is untested.

**P1 (next up):**

- No streaming yet (Phase 5) — CLI blocks until full completion.
- No `IterationBudget` (refund / grace call / subagent budget) — `LoopConfig.max_iterations` is a flat `u32`.
- `parallel_tool_calls` field is plumbed but always false.
- `Toolset` filtering works at registry construction time (via `--disabled-toolsets`) but is not reactive to per-turn changes.
