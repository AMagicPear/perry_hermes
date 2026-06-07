# AGENTS.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

当前进度：**Phase 0–10 已完成**（核心循环 + OpenAI/Anthropic 适配器 + BashTool + 运行时门面 + 交互式 CLI + 流式输出 + Ctrl-C 中断 + TOML provider/agent 配置 + Skills 加载 + Phase 7 上下文压缩 + Phase 10 `hermes-skill-loader` 重命名和 `hermes-cli` ratatui TUI 重写）。

**2026-06-06 重构**：原 `hermes-loop` / `hermes-runtime` / `hermes-tools` 三个 crate 合并为 `hermes-agent`（见 `docs/superpowers/specs/2026-06-06-crate-consolidation-design.md`）。workspace 现在 5 个 crate；旧的 import 路径（`hermes_loop::*`、`hermes_runtime::*`、`hermes_tools::*`）都不再可用。

## Build & Test

```bash
cargo build                              # all workspace crates
cargo test                               # all unit + integration tests
cargo test -p hermes-core                # single crate
cargo test -p hermes-agent --test tool_dispatch  # single integration test
cargo clippy --all-targets --all-features -- -D warnings

# Live smoke (needs provider env vars loaded; .envrc is available locally)
cargo run -p hermes-providers --example live_smoke
cargo run -p hermes-agent --example live_tool_use -- "what time is it?"
cargo run -p hermes-agent --example live_context_usage -- /Users/amagicpear/.perry_hermes/config.toml

# CLI smoke (offline, no API key needed)
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
echo "hello" | cargo run -p hermes-cli --quiet -- --config /tmp/hermes-smoke.toml
```

direnv auto-loads `.envrc` on `cd` — no manual env var exports needed once set up.

## Project-Specific Rules

- This project is experimental and has no users. When changing behavior or config shape, do **not** preserve old compatibility unless explicitly requested in the current task. 记住更改的时候不要考虑最小更改也不要考虑兼容性，完全用整体架构最优的思维去做更改。
- Current config format is `[[providers]]` plus `[[providers.models]]`; do not reintroduce the old single `[provider]` format.
- Provider selection lives in `[agent].default_provider` and `[agent].default_model`. CLI startup can override these for one run with `--provider <name>` and `--model <name>`.
- `context_window_size` belongs on each model entry under `[[providers.models]]`, not on `[agent]`.
- When real-provider testing is useful, use `/Users/amagicpear/.perry_hermes/config.toml` as the local config and load environment variables from `~/projects/perry_hermes/.envrc`.
- Real-provider checks should stay manual or in examples such as `crates/hermes-agent/examples/live_context_usage.rs`; do not put live provider calls into automated test scripts.
- Keep `AGENTS.md`, `README.md`, `examples/config/hermes.toml`, and `/Users/amagicpear/.perry_hermes/config.toml` aligned with the current config format.

## Architecture

Dependency direction is strictly downward — no reverse edges between crates. Workspace members (see root `Cargo.toml`):

```
hermes-cli (ratatui TUI — 正在替换原 REPL)
  └─ hermes-agent (AIAgent + AgentLoop + built-in tools + config + skills wiring)
       ├─ hermes-core (types, traits, errors — no IO)
       ├─ hermes-providers (OpenAI / Anthropic adapters, Echo mock)
       └─ hermes-skill-loader (SKILL.md data loading + system-prompt block)
```

`hermes-providers` and `hermes-skill-loader` are independent leaves — adding a tool or skill never touches a provider, and vice versa. `hermes-core` is also a leaf. Phase 11 计划新增 `hermes-gateway`（Slack/Discord/Telegram 适配器），与 `hermes-cli` 并列消费 `AIAgent`。

### Inside `hermes-agent`

`lib.rs` is intentionally tiny — module declarations and public re-exports only. The crate is split across these files:

| File | Responsibility |
|---|---|
| `src/lib.rs` | Module declarations + public re-exports only |
| `src/runtime_agent.rs` | `AIAgent` facade; `from_config` / `new` constructors, `run_turn` / `run_messages` |
| `src/loop_engine.rs` | `AgentLoop` state machine: `stream` → dispatch tools → repeat. Also `LoopConfig`, `LoopEvent`, `RunResult`, `LoopMetrics` |
| `src/config.rs` | `HermesConfig` / `ProviderConfig` / `ModelConfig` / `ResolvedProviderConfig` / `AgentConfig` / `ProviderKind` / `ThinkingConfig` + `HermesConfig::from_path` and `resolve_provider` |
| `src/session.rs` | `SessionContext` and per-run session metadata carrier |
| `src/prompting.rs` | Base prompt composition, runtime system-prompt injection, session/environment metadata formatting |
| `src/provider_factory.rs` | `build_provider(&ResolvedProviderConfig) -> Box<dyn Provider>` — single place that knows how to construct each provider from selected config |
| `src/tool_catalog.rs` | `build_registry(&[disabled_toolsets])` — wires built-in tools into an `InMemoryRegistry` |
| `src/tools/` | Built-in tool domains: `bash.rs`, `files/`, `skills/`, plus shared `support/` helpers |

Integration tests live in `crates/hermes-agent/tests/`: `echo_loop`, `tool_dispatch`, `arg_validation`, `usage_metrics`, `bash`, `skills_injection`. The `tests/support/mod.rs` `ScriptedProvider` returns a fixed sequence of `Completion`s for multi-iteration scenarios.

### Core boundaries

- `Provider` — async `stream(messages, tools, cancel) -> CompletionStream`. `ProviderError` via `?` propagates cleanly into `LoopError`. `Provider::complete` is a default impl that calls `accumulate_stream`.
- `Tool` — async `execute(args, ctx, cancel) -> ToolOutput`. `ToolError` is **non-fatal**: the loop wraps it in `role: tool` content so the LLM sees the error and can pivot. JSON-Schema args validation happens at dispatch time via `jsonschema` (Draft 7).
- `InMemoryRegistry` — maps tool names to `Arc<dyn Tool>`. The `tool_catalog` builder decides which tools register (gated by `disabled_toolsets`).
- `LoopEvent` — typed presentation events for CLI/gateway. The agent reports what happened; adapters decide how to render it.
- `StreamAccumulator` (`hermes-core::accumulator`) — turns a `CompletionStream` into a final `Completion` (or a partial `Message` for cancellation). Pure data, no async, no I/O.

**The agent loop** (`hermes-agent/src/loop_engine.rs`): `AgentLoop` holds `Arc<dyn Provider>` plus an `Arc<InMemoryRegistry>` and a `LoopConfig`. The `run()` method is a state machine — on `FinishReason::ToolUse` it dispatches tools sequentially, appends `role: tool` messages, and calls the provider again. `on_event` callback is the only side-channel (CLI uses it for spinner/activity feed; tests collect event strings). **`run()` takes a `ToolContext`** so runtime/gateway controls `session_id`, `working_dir`, and permissions.

**Runtime + CLI/TUI** (`hermes-agent/src/runtime_agent.rs`, `crates/hermes-cli/src/main.rs`): the runtime is the shared composition point for CLI/TUI and future gateway. `AIAgent::from_config(HermesConfig)` and `AIAgent::new(provider, HermesConfig)` are the two constructors. Per-run `working_dir` / `session_id` travel in a `SessionContext` passed to `run_turn` / `run_messages` (the runtime is reusable across sessions). The CLI binary only parses args, resolves the config path (`--config` → `~/.perry_hermes/config.toml` → `./hermes.toml`, error if none), optionally overrides `[agent].default_provider` and `[agent].default_model` with `--provider` / `--model`, and hands off to a `ratatui` event loop that drives the agent and renders `LoopEvent`s. Multi-turn: `run_result.messages` becomes the next turn's history. Ctrl-C: first cancels the current turn via `CancellationToken`, second exits the TUI. Slash commands (`/quit`, `/exit`, `/compact [focus]`) live inside the TUI's input handler. Provider-specific values live under `[[providers]]`; model names and context windows live under `[[providers.models]]`. `HermesConfig::resolve_provider()` selects the provider/model named by `[agent].default_provider` and `[agent].default_model`. The TUI displays the provider `name` (for example `minimax`), not provider `kind` (`anthropic`/`openai`). Skills live in `~/.perry_hermes/skills/`; the runtime loads them when `compose_system_prompt` is called (during `AIAgent::from_config`) and injects a name+description index into the system prompt. Context compression is enabled by default unless `[agent].context_compression_enabled = false`.

**OpenAI adapter** (`hermes-providers/src/openai.rs`): `OpenAiProvider::with_base_url()` lets it talk to any OpenAI-compatible endpoint (DeepSeek, MiniMax, Ollama, vLLM). The `tool_calls` field round-trips: assistant's `tool_calls` are serialized back into the next request body so the LLM remembers which tools it called. OpenAI sends `arguments` as a JSON **string** (not object) — must `serde_json::from_str` on parse and `to_string` on serialize. `tool_choice: "auto"` is **omitted** when the tool list is empty (some providers reject it). `stream_options.include_usage = true` is sent so `in`/`out` metrics populate. `finish_reason` is parsed via `FinishReason::from_provider_str` (renamed from `from_str` to avoid colliding with the std `FromStr` trait).

**Anthropic adapter** (`hermes-providers/src/anthropic.rs`): official `x-api-key` header by default; switch via `api_key_header` (e.g. MiMo uses `api-key`). `thinking` is **off** unless `[providers.thinking].mode` is set to `manual` or `adaptive` — third-party Anthropic-compatible endpoints should usually stay `off`.

## TDD Workflow

Strict RED→GREEN→REFACTOR. No production code without a failing test first.

- **RED**: write test, `cargo test`, confirm it fails for the right reason
- **GREEN**: minimal code to pass, `cargo test` again, all green
- **REFACTOR**: clean up, tests still green

For loop tests, `ScriptedProvider` (in `crates/hermes-agent/tests/support/`) returns a fixed sequence of `Completion`s — use it for multi-iteration scenarios. For OpenAI provider HTTP tests, `httpmock` mocks the server. For request body inspection, use a raw `tokio::net::TcpListener` (httpmock 0.7 doesn't expose captured bodies). When tests mutate process-wide state (e.g. `HOME` for CLI config resolution), serialize them with a static `Mutex<()>` (see `hermes-cli/src/main.rs` and `hermes-cli/tests/cli_smoke.rs` for the pattern). For TUI tests (Phase 10+), drive the `ratatui` `App` through a fixed-size `TestBackend` and assert on rendered `Buffer` content — no real terminal required.

## Design Docs

Current code is authoritative. Design docs live in `docs/superpowers/specs/` and `docs/superpowers/plans/`:

| File | Purpose |
|---|---|
| `2026-06-06-crate-consolidation-design.md` | 5-crate layout after merging `hermes-loop`/`hermes-runtime`/`hermes-tools` |
| `2026-06-06-architecture-cohesion-refactor-design.md` | Architecture cleanup and cohesion refactor |
| `2026-06-06-builtin-tools-expansion-design.md` | Built-in tools expansion design |
| `2026-06-06-phase-10-rename-and-tui-design.md` | Phase 10: `hermes-skills` → `hermes-skill-loader` + `hermes-cli` → ratatui TUI |
| `2026-06-06-phase-12-skill-view-tool-design.md` | SkillView tool design (Phase 12) |

`docs/history/hermes-comparison.md` tracks divergence from the Python Hermes source.

## Known Issues

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

- No `IterationBudget` (refund / grace call / subagent budget) — `LoopConfig.max_iterations` is a flat `u32` (default 10, set via `[agent].max_iterations` in TOML).
- `disabled_toolsets` is configured at startup via `[agent].disabled_toolsets` in the TOML config (e.g. `["terminal"]` to disable `BashTool`); it is not reactive to per-turn changes.
- Context compression is intentionally simple: default-on, one built-in compressor, and a smaller default protected tail so manual `/compact` is less likely to skip medium-length conversations.
