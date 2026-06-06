# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

当前进度：**Phase 0–6、Phase 8–9 已完成**（核心循环 + OpenAI/Anthropic 适配器 + BashTool + 运行时门面 + 交互式 CLI + 流式输出 + Ctrl-C 中断 + TOML provider/agent 配置 + Skills 加载）。Phase 7 上下文压缩仍暂缓。

**2026-06-06 重构**：原 `hermes-loop` / `hermes-runtime` / `hermes-tools` 三个 crate 合并为 `hermes-agent`（见 `docs/superpowers/specs/2026-06-06-crate-consolidation-design.md`）。workspace 现在 5 个 crate；旧的 import 路径（`hermes_loop::*`、`hermes_runtime::*`、`hermes_tools::*`）都不再可用。

## Build & Test

```bash
cargo build                              # all workspace crates
cargo test                               # all unit + integration tests
cargo test -p hermes-core                # single crate
cargo test -p hermes-agent --test tool_dispatch  # single integration test
cargo clippy --all-targets --all-features -- -D warnings

# Live smoke (needs OPENAI_API_KEY, OPENAI_BASE_URL, OPENAI_MODEL in env)
cargo run -p hermes-providers --example live_smoke
cargo run -p hermes-agent --example live_tool_use -- "what time is it?"

# CLI smoke (offline, no API key needed)
echo '[provider]\nkind = "echo"' > /tmp/hermes-smoke.toml
echo "hello" | cargo run -p hermes-cli --quiet -- --config /tmp/hermes-smoke.toml
```

direnv auto-loads `.envrc` on `cd` — no manual env var exports needed once set up.

## Architecture

Dependency direction is strictly downward — no reverse edges between crates. Workspace members (see root `Cargo.toml`):

```
hermes-cli (interactive REPL)
  └─ hermes-agent (AIAgent + AgentLoop + built-in tools + config + skills wiring)
       ├─ hermes-core (types, traits, errors — no IO)
       ├─ hermes-providers (OpenAI / Anthropic adapters, Echo mock)
       └─ hermes-skills (SKILL.md loading + system-prompt block)
```

`hermes-providers` and `hermes-skills` are independent leaves — adding a tool or skill never touches a provider, and vice versa. `hermes-core` is also a leaf.

### Inside `hermes-agent`

`lib.rs` is intentionally tiny — module declarations and public re-exports only. The crate is split across these files:

| File | Responsibility |
|---|---|
| `src/lib.rs` | Module declarations + public re-exports only |
| `src/runtime_agent.rs` | `AIAgent` facade + `SessionContext`; `from_config` / `new` constructors, `run_turn` / `run_messages` |
| `src/loop_engine.rs` | `AgentLoop` state machine: `stream` → dispatch tools → repeat. Also `LoopConfig`, `LoopEvent`, `RunResult`, `LoopMetrics` |
| `src/config.rs` | `HermesConfig` / `ProviderConfig` / `AgentConfig` / `ProviderKind` / `ThinkingConfig` + `HermesConfig::from_path` |
| `src/prompting.rs` | `DEFAULT_SYSTEM_PROMPT`, `compose_system_prompt` (skills-index injection happens here) |
| `src/provider_factory.rs` | `build_provider(&ProviderConfig) -> Box<dyn Provider>` — single place that knows how to construct each provider from config |
| `src/tool_catalog.rs` | `build_registry(&[disabled_toolsets])` — wires built-in tools into an `InMemoryRegistry` |
| `src/tools/bash.rs` | `BashTool` (only built-in tool today) |

Integration tests live in `crates/hermes-agent/tests/`: `echo_loop`, `tool_dispatch`, `arg_validation`, `usage_metrics`, `bash`, `skills_injection`. The `tests/support/mod.rs` `ScriptedProvider` returns a fixed sequence of `Completion`s for multi-iteration scenarios.

### Core boundaries

- `Provider` — async `stream(messages, tools, cancel) -> CompletionStream`. `ProviderError` via `?` propagates cleanly into `LoopError`. `Provider::complete` is a default impl that calls `accumulate_stream`.
- `Tool` — async `execute(args, ctx, cancel) -> ToolOutput`. `ToolError` is **non-fatal**: the loop wraps it in `role: tool` content so the LLM sees the error and can pivot. JSON-Schema args validation happens at dispatch time via `jsonschema` (Draft 7).
- `InMemoryRegistry` — maps tool names to `Arc<dyn Tool>`. The `tool_catalog` builder decides which tools register (gated by `disabled_toolsets`).
- `LoopEvent` — typed presentation events for CLI/gateway. The agent reports what happened; adapters decide how to render it.
- `StreamAccumulator` (`hermes-core::accumulator`) — turns a `CompletionStream` into a final `Completion` (or a partial `Message` for cancellation). Pure data, no async, no I/O.

**The agent loop** (`hermes-agent/src/loop_engine.rs`): `AgentLoop` holds `Arc<dyn Provider>` plus an `Arc<InMemoryRegistry>` and a `LoopConfig`. The `run()` method is a state machine — on `FinishReason::ToolUse` it dispatches tools sequentially, appends `role: tool` messages, and calls the provider again. `on_event` callback is the only side-channel (CLI uses it for spinner/activity feed; tests collect event strings). **`run()` takes a `ToolContext`** so runtime/gateway controls `session_id`, `working_dir`, and permissions.

**Runtime + CLI** (`hermes-agent/src/runtime_agent.rs`, `crates/hermes-cli/src/main.rs`): the runtime is the shared composition point for CLI and future gateway. `AIAgent::from_config(HermesConfig)` and `AIAgent::new(provider, HermesConfig)` are the two constructors. Per-run `working_dir` / `session_id` travel in a `SessionContext` passed to `run_turn` / `run_messages` (the runtime is reusable across sessions). CLI only parses args, resolves the config path (`--config` → `~/.perry_hermes/config.toml` → `./hermes.toml`, error if none), maintains REPL history, and renders `LoopEvent`s. Multi-turn: `run_result.messages` becomes the next turn's history. Ctrl-C: first cancels the current turn via `CancellationToken`, second exits the loop. Slash commands: `/quit`, `/exit`. Provider-specific things (`OPENAI_API_KEY` etc.) live in `[provider]` of the TOML file; the runtime never reads the environment for defaults except for the single key named by `api_key_env`. Skills live in `~/.perry_hermes/skills/`; the runtime loads them when `compose_system_prompt` is called (during `AIAgent::from_config`) and injects a name+description index into the system prompt.

**OpenAI adapter** (`hermes-providers/src/openai.rs`): `OpenAiProvider::with_base_url()` lets it talk to any OpenAI-compatible endpoint (DeepSeek, MiniMax, Ollama, vLLM). The `tool_calls` field round-trips: assistant's `tool_calls` are serialized back into the next request body so the LLM remembers which tools it called. OpenAI sends `arguments` as a JSON **string** (not object) — must `serde_json::from_str` on parse and `to_string` on serialize. `tool_choice: "auto"` is **omitted** when the tool list is empty (some providers reject it). `stream_options.include_usage = true` is sent so `in`/`out` metrics populate. `finish_reason` is parsed via `FinishReason::from_provider_str` (renamed from `from_str` to avoid colliding with the std `FromStr` trait).

**Anthropic adapter** (`hermes-providers/src/anthropic.rs`): official `x-api-key` header by default; switch via `api_key_header` (e.g. MiMo uses `api-key`). `thinking` is **off** unless `[provider.thinking].mode` is set to `manual` or `adaptive` — third-party Anthropic-compatible endpoints should usually stay `off`.

## TDD Workflow

Strict RED→GREEN→REFACTOR. No production code without a failing test first.

- **RED**: write test, `cargo test`, confirm it fails for the right reason
- **GREEN**: minimal code to pass, `cargo test` again, all green
- **REFACTOR**: clean up, tests still green

For loop tests, `ScriptedProvider` (in `crates/hermes-agent/tests/support/`) returns a fixed sequence of `Completion`s — use it for multi-iteration scenarios. For OpenAI provider HTTP tests, `httpmock` mocks the server. For request body inspection, use a raw `tokio::net::TcpListener` (httpmock 0.7 doesn't expose captured bodies). When tests mutate process-wide state (e.g. `HOME` for CLI config resolution), serialize them with a static `Mutex<()>` (see `hermes-cli/src/main.rs` and `hermes-cli/tests/cli_smoke.rs` for the pattern).

## Design Doc

Current code is authoritative. `docs/history/rust-port-design.md` is a historical design draft — useful for intent and roadmap, but some API sketches are stale after the runtime/streaming simplification and the 2026-06-06 crate consolidation. `docs/history/hermes-comparison.md` tracks divergence from the Python source and has a current-status note at the top.

Phase / refactor designs live in `docs/superpowers/specs/` and the execution plans in `docs/superpowers/plans/` (e.g. `2026-06-06-crate-consolidation-design.md` and its plan describe the current 5-crate layout).

## Known Issues (from comparison report)

**Resolved (in the codebase today):**

- ✅ BashTool pipe deadlock — `crates/hermes-agent/src/tools/bash.rs` drains `stdout` and `stderr` concurrently via `tokio::join!` and uses `tokio::select!` on cancel/timeout.
- ✅ `tool_choice: "auto"` sent on empty tool list — OpenAI provider sets `tool_choice: None` when `tools.is_empty()`.
- ✅ No streaming — `Provider::stream()` is the only required method; `OpenAiProvider` parses SSE and yields `CompletionDelta`; `AgentLoop::run` drives the stream via `tokio::select!` and emits `ContentDelta` / `ReasoningDelta` / `ToolCallPartial` events. CLI prints tokens as they arrive. Cancel mid-stream preserves partial content via `LoopError::CancelledWith(Message)`.
- ✅ Over-abstracted registry/loop — removed `ToolRegistry` trait and `AgentLoop<P, R>` generics; current loop uses `Arc<dyn Provider>` + `InMemoryRegistry`.
- ✅ Runtime facade too thin — CLI goes through `hermes_agent::AIAgent`; the runtime is the shared composition point for future gateway.
- ✅ Fragmented runtime crates — `hermes-loop` / `hermes-runtime` / `hermes-tools` merged into `hermes-agent` (2026-06-06).

**Still open:**

- `ToolContext.permissions` is **not enforced** — `BashTool` checks `subprocess`, but `AIAgent` always grants it. No finer gateway policy yet.
- Unknown `finish_reason` maps to `FinishReason::Error`, but provider diagnostics are still coarse.
- OpenAI-compatible providers now serialize `Content::Parts` as text/image_url content arrays; other media part types are not modeled yet.
- `BashTool` does not kill concurrent children — under heavy parallel load the `child.kill().await` path is untested.

**P1 (next up):**

- No `IterationBudget` (refund / grace call / subagent budget) — `LoopConfig.max_iterations` is a flat `u32` (default 10, set via `[agent].max_iterations` in TOML). (Phase 7)
- `disabled_toolsets` is configured at startup via `[agent].disabled_toolsets` in the TOML config (e.g. `["terminal"]` to disable `BashTool`); it is not reactive to per-turn changes.
- Context compression (Phase 7).
