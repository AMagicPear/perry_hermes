# Readability & Maintainability Refactor Design

**Date:** 2026-06-07
**Status:** Approved
**Scope:** All 5 crates; src/, tests/, examples/. No `rust_out/` or root binaries.
**Goal:** Real human maintainers can follow the code top-to-bottom and understand what each piece is doing and why, with no special-casing.

## Motivation

The previous cleanup pass (2026-06-07) removed dead code. This pass goes deeper: it reshapes the *live* code so that the boundaries between pieces are obvious and each piece does one thing. The driving complaint is "I open a file and I can't tell where one concept ends and another begins."

Concretely, the following pain points surfaced when re-reading the source top-to-bottom:

1. `loop_engine.rs` is 566 lines and `try_compress` drops and re-locks the context engine mid-function with a comment admitting it should be restructured. The `run()` method is a single 200-line state machine that interleaves cancellation checks, compression pre/post-turn checks, tool dispatch, and finish-reason handling.
2. `openai.rs` (603 lines) and `anthropic.rs` (896 lines) each mix four concerns: HTTP client, request DTOs, SSE parser, and provider impl. The DTO naming is inconsistent (`OaiMessage` vs `WireMessage`). Tests inside the same file fight the public module surface.
3. `compressor.rs` is 716 lines; it owns strategy, summarization, and message-finding in one module.
4. `tui/{render,app,input,run}.rs` total 1766 lines; `input.rs::handle_key` is a 100-line match that mixes global key bindings (Ctrl-C, Esc), mode-keyed scroll bindings, and per-mode text editing into one match.
5. The `Content` enum (`Text(String)` / `Parts(Vec<ContentPart>)`) is matched manually across the codebase; `as_text().contains(...)`-style hacks (e.g. `accumulator.rs:108` looking for `[CONTEXT SUMMARY` inside text) leak the data layout into every consumer.
6. The accumulator's `arguments_delta` is a JSON `String` that gets re-parsed in two places (`parse_string_arguments` and the `into_partial_message` retain filter). The double-parse is a maintenance trap.
7. Several `if cond { None } else { Some(build_vec()) }` patterns in `accumulator.rs` and `runtime_agent.rs` obscure the simple "this is an Option" idea.

The principle: **for every code shape that surprised me while reading, write down the surprise and remove the surprise.** A maintainer should be able to jump into any single file, see the file's purpose in the top-level doc comment, find each function/struct in a few seconds, and understand the function's behavior from the body without scrolling up to consult module-level helpers.

## Approach

Five passes, each scope-bounded so the working tree stays green between them. Each pass ends with `cargo test --workspace` and `cargo clippy --all-targets --all-features -- -D warnings` clean.

**Verification gates (run after every pass, no exceptions):**

- `cargo test --workspace` — all 284+ tests green
- `cargo clippy --all-targets --all-features -- -D warnings` — zero warnings
- `cargo doc --no-deps` — builds clean (catches broken doc links after renames)

### Pass 1 — `hermes-core`: tighten the data layer

`hermes-core` is the foundation; everything depends on its types. Get this right first.

- **`Content` ergonomics.** Add `Content::as_text(&self) -> Option<&str>` (already exists) and a `Content::chars(&self) -> usize` that flattens `Text` + `Parts` into one number. Add `Content::is_text(&self) -> bool`. Replace manual `match &m.content` blocks with these accessors in `loop_engine.rs`, `compressor.rs`, `accumulator.rs`, and `accumulator.rs:108`'s `[CONTEXT SUMMARY` substring check (which is a clear smell — the compressor should be able to ask "does this message contain the summary marker?" without knowing the content layout).
- **`Message::char_len`.** Currently it serializes `tool_calls.arguments` back to a `String` for every call (line 92 of `message.rs`). Cache the per-call stringified args once, or expose a `char_len_into(&self, &mut usize)` so the compressor's tight loops stop re-allocating.
- **`ToolCall`.** Add `ToolCall::new(id, name, args) -> Self` constructor (avoid the 5-line literal at `accumulator.rs:48`, `runtime_agent.rs:255`, and tests).
- **`ToolCallDelta` → argument fragment naming.** Rename `arguments_delta: Option<String>` to `arguments_fragment: Option<&str>` (lifetime-carried to avoid the round-trip String in `accumulator.rs:75`). Internally we re-parse the concatenated string back to `Value` at the end of the stream, so renaming clarifies that the field is *not* a complete JSON value.
- **`Provider` trait doc comment.** Each method gets a 2-3 line "what this returns, when to call it" doc so an implementer doesn't have to read other implementations.
- **`Usage`.** Add a `+` (or `saturating_add`) impl and a `total() -> u64` helper; `loop_engine.rs:539` and `accumulator.rs:135` both do `usage.input_tokens.saturating_add(usage.cached_input_tokens)` manually.
- **No new types.** This pass does not add new variants or remove public ones. It just adds convenience, fixes the char-len bug, and renames one field.

### Pass 2 — `hermes-providers`: split OpenAI / Anthropic adapters

Both providers mix four concerns. Split each into a sub-module layout:

```
crates/hermes-providers/src/
  openai/
    mod.rs          -- OpenAiProvider, the Provider impl
    request.rs      -- ChatRequest + OaiMessage/OaiTool/...
    sse.rs          -- parse_sse_chunks + parse_sse_data_payload + state
  anthropic/
    mod.rs          -- AnthropicProvider, the Provider impl
    request.rs      -- MessagesRequest + WireMessage/...  (rename → Anthropic*)
    sse.rs          -- Anthropic SSE parsing
  echo.rs           -- already small, leave alone
  lib.rs            -- pub use the two top-level providers
```

**Naming.** Rename all `Wire*` → `Anthropic*` (`WireMessage` → `AnthropicMessage`, `WireContentBlock` → `AnthropicContentBlock`, `WireTool` → `AnthropicTool`, `WireToolChoice` → `AnthropicToolChoice`, `ThinkingParam` → `AnthropicThinkingParam`). Update all 5+ call sites. Same idea for OpenAI: rename `Oai*` → `Openai*` (OpenAI is the right brand; "OAI" is implementation jargon).

**SSE parsers.** Extract `split_think_tag_content` from `openai.rs` into a private helper that takes a state machine and the content string; it's currently a 30-line standalone function with its own comments. Move all SSE parsing tests into the new `sse.rs` module's `#[cfg(test)] mod tests`.

**`build_request_body`.** In OpenAI the function is 50 lines with two `map` calls and a `serde_json::to_string` fallback. Extract two helpers: `to_oai_message(m: &Message) -> OaiMessage` and `to_oai_tool(t: &ToolSchema) -> OaiTool`. Anthropic's `convert_messages_to_anthropic` and `convert_tools` are already separate — keep them.

**SSE chunk splitting.** Add a tiny helper `fn parse_sse_data_line(line: &str) -> Option<&str>` so `parse_sse_chunks` doesn't inline `line.strip_prefix("data: ")` and `trim()` in a closure.

### Pass 3 — `hermes-agent`: split `loop_engine.rs` and `compressor.rs`

#### 3a — `loop_engine.rs` (566 → ~350 lines)

Extract into three sibling modules under `crates/hermes-agent/src/loop_engine/`:

```
crates/hermes-agent/src/loop_engine/
  mod.rs        -- AgentLoop, LoopConfig, LoopEvent, RunResult, the public surface
  run.rs        -- the state machine: AgentLoop::run() impl (split into handle_iteration, handle_finish_reason, handle_tool_use)
  compress.rs   -- AgentLoop::try_compress + helpers
  metrics.rs    -- LoopMetrics + prompt_context_tokens_from_usage + the two estimate_* functions
  provider_failure.rs -- the FailedTurn builder, with a struct instead of 4 positional args
```

The `try_compress` lock dance (`drop(guard)` then `engine.lock().await` to get a `&mut self`) becomes a single function `compress_with(engine, messages, ...)` that takes the lock exactly once. The current code admits the issue in a comment; the fix is to scope the mutable borrow.

The 4-argument `provider_failure` becomes a struct construction so the call site reads as `FailedTurn::from(error, partial_message, initial_history_len, current_messages)`.

#### 3b — `runtime_agent.rs`: simpler construction

`build_loop_for_custom_provider` is 50 lines with deep nesting. Break into:

```rust
fn build_loop_for_custom_provider(...) -> AgentLoop {
    let skills_dir = resolve_skills_dir().unwrap_or_else(default_skills_dir);
    let registry = Arc::new(build_registry(&config.agent.disabled_toolsets, &skills_dir));
    let context_engine = build_context_engine(&config, &provider, selected_provider);
    let config = LoopConfig { max_iterations: ..., .. };
    AgentLoop::from_provider(provider, registry, config)
}
```

Each helper is 5-10 lines and named for what it does. `default_skills_dir` is a top-level `fn` so the test for the CWD fallback is a one-liner.

Drop the `#[allow(clippy::needless_pass_by_value)]` on `AIAgent::from_config` and `AIAgent::new` — take them by value still (clippy is wrong; the docs and tests depend on by-value construction). Or, since the user said "完全自由": keep them by value but write a one-line doc comment on each constructor explaining *why* it's by value (move semantics for owned strings, single-construction at startup).

#### 3c — `compressor.rs` (716 → ~250 lines, four sub-modules)

```
crates/hermes-agent/src/context/
  compressor/
    mod.rs        -- ContextCompressor, the top-level type + CompressorConfig
    strategy.rs   -- pick_messages_to_compress: which messages are eligible
    summary.rs    -- call the LLM to produce the summary text + token budget enforcement
    marker.rs     -- the [CONTEXT SUMMARY ...] marker format + parser/validator
```

The `[CONTEXT SUMMARY` substring check moves to `marker.rs` as `is_summary_message(message: &Message) -> bool` and `summary_chars(messages: &[Message]) -> usize`. `accumulator.rs:108` and `compressor.rs` both call this; now there is one source of truth.

The summarization step currently builds a prompt by string concatenation. Extract a `build_summary_prompt(messages_to_summarize, focus_topic, model_context) -> String` and a `parse_summary_response(text: &str) -> String` so the LLM-call boundary is one function.

#### 3d — `tools/` clean-up

`tools/files/policy.rs` has 311 lines mixing path-sensitivity, profile scoping, and dedup detection. Split into:

```
crates/hermes-agent/src/tools/files/
  policy/
    mod.rs        -- re-exports + high-level "should we block this path?" entry
    sensitive.rs  -- the exact-blocked / prefix-blocked / hermes-config checks
    profile.rs    -- cross_profile_write_message + current_profile_from_hermes_home
    dedup.rs      -- is_internal_file_status_text
```

`tools/skills/` (already split into list.rs, view.rs, linked_files.rs) is fine.

### Pass 4 — `hermes-cli`: TUI ergonomics

#### 4a — `input.rs` (428 → ~200 lines)

`handle_key` is 100+ lines of nested match. Refactor into a top-level `dispatch_key(app, key) -> AppEvent` that calls mode-keyed helpers:

```rust
pub fn dispatch_key(app: &mut App, key: KeyEvent) -> AppEvent {
    // 1. Global modifiers (Ctrl-C, Ctrl-D, Esc) — checked first
    if let Some(event) = try_global_key(app, key) { return event; }
    // 2. In Cancelling mode: ignore everything else
    if app.mode == AppMode::Cancelling { return AppEvent::Tick; }
    // 3. Chat scroll keys (Idle only)
    if app.mode == AppMode::Idle {
        if let Some(event) = try_chat_scroll_key(app, key) { return event; }
    }
    // 4. Text editing keys
    match key.code { ... }
}

fn try_global_key(app: &App, key: KeyEvent) -> Option<AppEvent> { ... }
fn try_chat_scroll_key(app: &mut App, key: KeyEvent) -> Option<AppEvent> { ... }
```

Each helper is 5-15 lines and named for one concern. The same global `match` for `KeyCode` becomes a 10-line `match` that only deals with text editing.

#### 4b — `render.rs` (581 → ~300 lines)

Split into:

```
crates/hermes-cli/src/tui/render/
  mod.rs        -- pub fn render(frame, app) — the top-level draw call
  layout.rs     -- the main/chat/input/status block split (split into Rect regions)
  chat.rs       -- build_chat_lines and friends
  input.rs      -- render the input box
  status.rs     -- render the status bar
```

The chat-line cache (currently in `App`) is rendering state, not app state. Move it into a `ChatView` struct in `render/chat.rs` that wraps the scrollback and caches wrapped lines per (width, revision).

#### 4c — `app.rs` and `run.rs`

`App` (379 lines) is mostly the right size. The `app.rs` test-only fields (`scrollback_revision`, `cached_chat_*`) move to `ChatView` (see 4b). After the move, `App::new_for_test()` becomes a 5-line struct literal.

`run.rs` (378 lines) — production `run` is 100 lines, test `run_with_backend` is similar length. Extract the keypress / event-stream setup into a `tui/event_loop.rs` module so `run.rs` becomes:

```rust
pub async fn run(...) -> Result<(), RunError> {
    let backend = setup_terminal()?;
    let (input_tx, input_rx) = setup_input();
    let result = event_loop::run(terminal, app, input_rx, agent, cancel).await;
    restore_terminal()?;
    result
}
```

The 5-page `mod.rs` of `tui/` becomes a tidy index of `pub use` lines.

### Pass 5 — naming & final polish (whole workspace)

After the four structural passes, sweep for naming inconsistencies and tighten doc comments. This pass is the "polish":

- **DTO prefix uniformity.** Already done in Pass 2. Verify every reference.
- **Test helpers.** `crates/hermes-providers/tests/openai.rs::user_message` and `crates/hermes-providers/tests/anthropic.rs::message` are two names for the same shape. Pick one (`user_message` is more descriptive of the test intent — most tests pass `Role::User`) and update the Anthropic test. Same for any other cross-file test helpers.
- **Magic numbers.** `4.0` (chars per token) in `loop_engine.rs` and `compressor.rs` → a named const `CHARS_PER_TOKEN_ESTIMATE: f64 = 4.0` in `hermes-core` so the heuristic has a name and one source.
- **`if cond { None } else { Some(build()) }` patterns.** Convert to `.filter(|x| cond).map(...)` or `Option::from()` patterns. Targets: `accumulator.rs:103`, `accumulator.rs:115`, `runtime_agent.rs:113`, `compressor.rs` (a few places).
- **Doc comments.** Every pub item in `lib.rs` files has a one-line summary. The `//` and `///` headers that just repeat the function name get shortened or removed.
- **`#[allow(...)]` audit.** Every existing `#[allow]` gets a one-line comment explaining *why* it's there. Drop any that are stale. The two current allows (`redundant_clone` on `loop_engine.rs:478`, `needless_pass_by_value` on `runtime_agent.rs:42`) are correct — add the explanation.

## File-Level Risk Inventory

| File | Pass | Risk | Mitigation |
|---|---|---|---|
| `hermes-core/src/message.rs` | 1 | Public field rename on `ToolCallDelta` | Only one internal consumer (`accumulator.rs`); rename + update in same PR |
| `hermes-providers/src/openai.rs` | 2 | Test file imports private items | Add `pub(super)` carefully; tests live in `tests/` dir so we only need public re-exports |
| `hermes-providers/src/anthropic.rs` | 2 | `AnthropicThinking` is pub; `Wire*` was not | Add `Anthropic*` aliases + mark old names deprecated or just delete (user said 完全自由) |
| `hermes-agent/src/loop_engine.rs` | 3a | Public type re-exports in `lib.rs` | Keep `pub use loop_engine::*` and re-export the new sub-modules' surface from the old paths via `pub mod loop_engine` instead of `mod loop_engine` |
| `hermes-agent/src/compressor.rs` | 3c | Field on `CompressorConfig` | The struct stays; helper fns move out |
| `hermes-cli/src/tui/input.rs` | 4a | `handle_key` is the public API for tests | Keep `handle_key` as a thin wrapper around `dispatch_key` so existing tests compile |

## Verification

After every pass:

```bash
cargo test --workspace                                            # all 284+ tests green
cargo clippy --all-targets --all-features -- -D warnings          # zero warnings
cargo doc --no-deps                                              # clean
```

After every pass also rebuild the doc and skim the changed files' doc comments — broken doc links are a fast signal of incomplete renames.

Before declaring done: run the live smoke tests if a provider env is available:

```bash
cargo run -p hermes-agent --example live_tool_use -- "what time is it?"
```

(skip if no API key is configured; the 284+ unit tests cover the behavior already)

## Out of Scope

- **Performance.** No algorithm changes, no allocation profiling. This is a *readability* pass.
- **New features.** No new tools, no new provider adapters, no new LoopEvents.
- **Test coverage.** No new tests beyond what each pass needs to prove its refactor preserves behavior. Existing tests are the safety net.
- **External docs.** `README.md` and `docs/superpowers/specs/` outside this file are not touched. CLAUDE.md and `examples/config/hermes.toml` may need 1-line updates if a public type name changes; do that inline.
- **Behavior changes.** The TUI's key bindings, the OpenAI SSE parser's exact delta shape, and the agent loop's cancellation semantics are *not* touched.

## Success Criteria

- No single file in `crates/*/src/` exceeds ~500 lines (the 4 largest today: anthropic.rs 896, compressor.rs 716, openai.rs 603, render.rs 581 — all shrink to <= 400).
- The top-of-file doc comment of every file is a one-paragraph description of what this file does and what concerns it owns.
- Every public type / function in `lib.rs` has a one-line doc comment that says *what it does*, not *how*.
- `cargo test --workspace` is still 284+ tests green. `cargo clippy --all-targets --all-features -- -D warnings` is still clean.
- A new maintainer can open `crates/hermes-cli/src/tui/`, read `mod.rs`, and find any concern (key handling, rendering, event loop, app state) within 5 seconds.
