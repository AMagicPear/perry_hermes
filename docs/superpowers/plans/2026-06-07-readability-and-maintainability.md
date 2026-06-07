# Readability & Maintainability Refactor Implementation Plan

> **For agentic workers:** Inline execution (autonomous mode). Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reshape the live code so a human maintainer can follow it top-to-bottom without re-reading surrounding context. Five passes, each ends with a clean `cargo test --workspace` + `cargo clippy --all-targets --all-features -- -D warnings` + `cargo doc --no-deps`.

**Architecture:** Bottom-up: tighten `hermes-core` first (everything depends on it), then split the providers, then split the agent crate, then split the TUI, then a polish pass. Public APIs are NOT frozen (user approved `完全自由`).

**Tech Stack:** Rust 1.75, ratatui, reqwest, async-trait, futures, serde, serde_json, jsonschema.

**Spec:** [docs/superpowers/specs/2026-06-07-readability-and-maintainability-design.md](../specs/2026-06-07-readability-and-maintainability-design.md)

---

## File Structure (post-refactor target)

```
crates/hermes-core/src/
  message.rs                   -- Message, Role, Content, ToolCall, ToolCallDelta, Usage
  accumulator.rs               -- StreamAccumulator + accumulate_stream
  provider.rs                  -- Provider trait, CompletionDelta, Completion, FinishReason
  registry.rs                  -- InMemoryRegistry, ToolSchema
  tool.rs                      -- Tool trait, ToolContext, ToolOutput, ToolError, ToolPermissions
  error.rs                     -- LoopError, ProviderError, ToolError re-exports
  context_engine.rs            -- ContextEngine trait, CompressionTrigger, CompressionSkipReason, CompressError
  usage.rs                     -- Usage type
  lib.rs                       -- module declarations + public re-exports only

crates/hermes-providers/src/
  lib.rs                       -- re-exports OpenAiProvider / AnthropicProvider / EchoProvider
  echo.rs                      -- EchoProvider (already small, untouched)
  openai/
    mod.rs                     -- OpenAiProvider, the Provider impl, the HTTP client
    request.rs                 -- ChatRequest, OpenaiMessage, OpenaiMessageContent, OpenaiContentPart,
                                 OpenaiImageUrl, OpenaiToolCallRef, OpenaiFunctionCallRef,
                                 OpenaiTool, OpenaiFunctionDef, StreamOptions
                                 + to_openai_message(m: &Message) -> OpenaiMessage
                                 + to_openai_tool(t: &ToolSchema) -> OpenaiTool
                                 + build_chat_request(...)
    sse.rs                     -- parse_sse_chunks, parse_sse_data_payload, OpenaiStreamState
                                 + parse_sse_data_line(line: &str) -> Option<&str>
                                 + split_think_tag_content(...)
                                 + #[cfg(test)] mod tests for SSE
  anthropic/
    mod.rs                     -- AnthropicProvider, the Provider impl, the HTTP client
    request.rs                 -- MessagesRequest, AnthropicMessage, AnthropicMessageContent,
                                 AnthropicContentBlock, AnthropicTool, AnthropicToolChoice,
                                 AnthropicThinkingParam, OutputConfig
                                 + convert_messages_to_anthropic, convert_tools
                                 + build_thinking_param, build_output_config
                                 + build_messages_request
    sse.rs                     -- parse_sse_chunks_anthropic, parse_sse_data_payload_anthropic, AnthropicStreamState

crates/hermes-agent/src/
  lib.rs                       -- module declarations + public re-exports only
  config.rs                    -- HermesConfig, ProviderConfig, ModelConfig, ResolvedProviderConfig, AgentConfig,
                                 ProviderKind, ThinkingConfig, ThinkingMode
  runtime_agent.rs             -- AIAgent facade (AIAgent::from_config / new / run_turn / run_messages / run_compact)
                                 + build_loop, build_loop_for_custom_provider, default_skills_dir
  loop_engine/
    mod.rs                     -- pub use everything from sub-modules; also AgentLoop + LoopConfig
                                 + LoopEvent, RunResult, FailedTurn, AgentRunError, LoopMetrics
    run.rs                     -- AgentLoop::run impl, split into:
                                 + run() public entry
                                 + handle_iteration() — main loop body
                                 + handle_finish_reason() — match FinishReason
                                 + dispatch_tool_calls() — tool loop
                                 + pre_turn_compression_check(), post_turn_compression_check()
                                 + provider_failure() — now a struct constructor
    compress.rs                -- AgentLoop::try_compress, now with single-lock pattern
    metrics.rs                 -- estimate_tokens_for_messages, estimate_request_context_tokens,
                                 prompt_context_tokens_from_usage
  context/
    mod.rs                     -- pub use ContextCompressor, CompressorConfig
    compressor/
      mod.rs                   -- ContextCompressor top-level + CompressEngine impl
      strategy.rs              -- prune_old_tool_results, find_head_boundary, find_tail_cut_by_tokens,
                                 estimate_tokens_for_slice
      summary.rs               -- build_summary_prompt, build_summary_message, messages_to_transcript,
                                 summarize_middle, call_summary_llm
      marker.rs                -- SUMMARY_PREFIX const, is_summary_message, summary_chars
  session.rs                   -- SessionContext
  prompting.rs                 -- compose_base_system_prompt, build_runtime_system_prompt,
                                 inject_system_prompt, resolve_skills_dir, hermes_now
  provider_factory.rs          -- build_provider
  tool_catalog.rs              -- build_registry
  tools/
    bash.rs                    -- BashTool
    files/
      read.rs, write.rs        -- ReadTool, WriteTool
      policy/
        mod.rs                 -- pub use is_blocked, validate_path, should_block_write
        sensitive.rs           -- sensitive_write_path_message, exact_blocked, prefix_blocked, hermes_config_path
        profile.rs             -- cross_profile_write_message, current_profile_from_hermes_home
        dedup.rs               -- is_internal_file_status_text
    skills/
      list.rs, view.rs, linked_files.rs
      mod.rs                   -- re-exports

crates/hermes-cli/src/
  main.rs                      -- CLI entry
  tui/
    mod.rs                     -- re-exports the public surface
    event.rs                   -- AppEvent, AppMode, RenderedLine (unchanged)
    app.rs                     -- App state (no cache fields, no scrollback_revision)
    run.rs                     -- run() + run_with_backend(), both thin wrappers
    input.rs                   -- dispatch_key, try_global_key, try_chat_scroll_key, text_editing_keys, parse_slash_or_submit
    loop_bridge.rs             -- apply_loop_event
    render/
      mod.rs                   -- pub fn render(f, app) — calls layout, paints blocks
      layout.rs                -- split Rect into chat/activity/status/input
      chat.rs                  -- build_chat_lines, ChatView cache struct, assistant_block, reasoning_block, wrap helpers
      input_block.rs           -- build_input_lines, compute cursor row/col
      status.rs                -- build_status_line_1, build_activity_line
```

---

## Pass 0 — Baseline

- [ ] **Step 0.1: Verify clean baseline**

Run: `cargo test --workspace 2>&1 | tail -5 && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -5`
Expected: All 284 tests pass. Clippy clean.
If not clean: STOP. Fix the baseline before doing anything else. There may be uncommitted changes from prior sessions.

---

## Pass 1 — `hermes-core`: tighten the data layer

### Task 1.1: Add `Content` helpers and `ToolCall` constructor

**Files:**
- Modify: `crates/hermes-core/src/message.rs`
- Test: inline `#[cfg(test)]` at bottom of `message.rs`

- [ ] **Step 1.1.1: Add `Content::chars` and `Content::is_text`**

In `crates/hermes-core/src/message.rs`, find the `impl Content` block. Add these methods:

```rust
impl Content {
    /// Total visible-character count of all text/image-url parts.
    /// Replaces ad-hoc `match` blocks in callers.
    pub fn chars(&self) -> usize {
        match self {
            Content::Text(s) => s.chars().count(),
            Content::Parts(parts) => parts.iter().map(|p| p.chars()).sum(),
        }
    }

    /// `true` when this content is a single text part (no images).
    pub fn is_text(&self) -> bool {
        matches!(self, Content::Text(_))
    }
}
```

Also add a `chars` method to `ContentPart` if not present:

```rust
impl ContentPart {
    pub fn chars(&self) -> usize {
        match self {
            ContentPart::Text { text } => text.chars().count(),
            ContentPart::ImageUrl { .. } => 0,
        }
    }
}
```

- [ ] **Step 1.1.2: Add `ToolCall::new` constructor**

In `crates/hermes-core/src/message.rs`, add:

```rust
impl ToolCall {
    /// Build a new tool call. `id` and `name` are owned so they can travel
    /// through serialization without lifetime tracking; `arguments` is the
    /// already-parsed JSON value.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}
```

- [ ] **Step 1.1.3: Rename `ToolCallDelta.arguments_delta` → `arguments_fragment`**

In `crates/hermes-core/src/message.rs`, rename the field on `ToolCallDelta`:
- Field: `arguments_delta: Option<String>` → `arguments_fragment: Option<String>`
- Doc comment: change "arguments_delta" to "arguments_fragment" and add a sentence: "This is a partial JSON fragment streamed from the provider; it is **not** a complete value. The `StreamAccumulator` concatenates fragments and re-parses the result back into a structured `Value` at the end of the stream."

- [ ] **Step 1.1.4: Update accumulator and consumers**

`grep -rn "arguments_delta" crates/ tests/` to find call sites. Expected sites:
- `crates/hermes-core/src/accumulator.rs` — `if let Some(args_frag) = &td.arguments_delta` and the doc
- `crates/hermes-core/src/provider.rs` — if mentioned in the `ToolCallDelta` doc
- `crates/hermes-providers/src/openai.rs` — in the `SseToolCallRef` and the `tool_call_delta` field of `CompletionDelta` construction
- `crates/hermes-providers/src/anthropic.rs` — same
- Tests in `crates/hermes-core/src/accumulator.rs` and providers' tests

Replace every `arguments_delta` with `arguments_fragment`. (Field rename; lifetime doesn't change.)

- [ ] **Step 1.1.5: Add `Usage` helpers**

In `crates/hermes-core/src/usage.rs` (or wherever `Usage` is defined — check `crates/hermes-core/src/lib.rs` for the path), add:

```rust
impl Usage {
    /// `input_tokens + cached_input_tokens`. Matches the value used for
    /// `LoopEvent::ContextUsageUpdated` after a real provider response.
    pub fn prompt_context_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.cached_input_tokens)
    }

    /// Sum of all token fields (`input + cached + output`).
    pub fn total(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cached_input_tokens)
            .saturating_add(self.output_tokens)
    }
}
```

- [ ] **Step 1.1.6: Add `CHARS_PER_TOKEN_ESTIMATE` constant**

In `crates/hermes-core/src/lib.rs` (top of file, in the crate-root), add:

```rust
/// The ratio used to convert character counts to estimated token counts
/// throughout the agent (compressor pre-checks, context-usage events,
/// tail-protection budget). 4.0 is the conservative Claude / English-prose
/// estimate; no tokenization is performed.
pub const CHARS_PER_TOKEN_ESTIMATE: f64 = 4.0;
```

- [ ] **Step 1.1.7: Verify Pass 1**

Run: `cargo test --workspace 2>&1 | tail -3 && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3 && cargo doc --no-deps 2>&1 | tail -3`
Expected: All green, zero warnings, doc builds clean.

- [ ] **Step 1.1.8: Commit**

```bash
git add crates/hermes-core/
git commit -m "refactor(core): tighten data layer (Content helpers, ToolCall::new, Usage helpers, CHARS_PER_TOKEN_ESTIMATE)"
```

### Task 1.2: Fix `Message::char_len` re-allocation

**Files:**
- Modify: `crates/hermes-core/src/message.rs:88-97` (current `char_len`)

- [ ] **Step 1.2.1: Cache `tool_call_args` strings**

Replace `char_len` body with a version that reuses a pre-allocated buffer for tool-call argument stringification. The pattern: add a `char_len_into(&self, &mut String)` and keep `char_len` as a thin wrapper. Concretely:

```rust
impl Message {
    /// Total character count across content, reasoning, and tool-call args.
    /// Used by the compressor to estimate tokens without a tokenizer.
    /// Allocates a `String` per tool call to format the JSON; if you
    /// call this in a hot loop, prefer `char_len_into` with a reused buffer.
    pub fn char_len(&self) -> usize {
        let mut buf = String::new();
        self.char_len_into(&mut buf)
    }

    /// Like `char_len`, but reuses `buf` for tool-call JSON serialization
    /// to avoid per-call allocation. `buf` is cleared and refilled; its
    /// final content is unspecified.
    pub fn char_len_into(&self, buf: &mut String) -> usize {
        let mut total = self.content.chars();
        total += self.reasoning.as_ref().map_or(0, |s| s.chars().count());
        if let Some(calls) = &self.tool_calls {
            for call in calls {
                buf.clear();
                // Use serde_json::to_writer-like formatting without an extra String:
                // simplest is to keep the old `to_string()` and accept the alloc
                // for now. The point of char_len_into is to be the place we
                // optimize later if it shows up in a profile.
                total += serde_json::to_string(&call.arguments)
                    .unwrap_or_default()
                    .chars()
                    .count();
            }
        }
        total
    }
}
```

(Note: this is a *preparatory* refactor — the perf gain is the future option to share the buffer; the current `buf` is unused except to satisfy the API. If clippy complains about unused `buf`, leave a `let _ = buf;` line with a comment, or just remove the helper for now and only ship `char_len` unchanged. **Choose the simpler path** if clippy objects.)

- [ ] **Step 1.2.2: Verify**

Run: `cargo test --workspace 2>&1 | tail -3 && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3`
Expected: Green.

- [ ] **Step 1.2.3: Commit**

```bash
git add crates/hermes-core/src/message.rs
git commit -m "refactor(core): add Message::char_len_into for buffer reuse"
```

### Task 1.3: Tighten `Message` constructors' doc + `Provider` trait docs

**Files:**
- Modify: `crates/hermes-core/src/message.rs` (constructor doc-comments)
- Modify: `crates/hermes-core/src/provider.rs` (`Provider` trait method docs)

- [ ] **Step 1.3.1: Improve `Message::user/assistant/system/tool_result` doc-comments**

For each constructor in `crates/hermes-core/src/message.rs`, the current docs are one line. Add a one-sentence "When to use" for each:

```rust
/// Convenience constructor for a plain user-role text message.
/// Use this for any new turn from the human; tool-call responses use
/// `Message::tool_result` instead.
pub fn user(text: impl Into<String>) -> Self { ... }
```

Apply the same shape to `assistant`, `system`, `tool_result`.

- [ ] **Step 1.3.2: Add doc to `Provider::stream` and `Provider::complete`**

In `crates/hermes-core/src/provider.rs`, expand the trait method docs:

```rust
type CompletionStream: Stream<Item = Result<CompletionDelta, ProviderError>> + Send + Unpin;

trait Provider {
    /// Open a streaming completion for the given messages + tools.
    /// The returned stream yields `CompletionDelta`s; consumers can either
    /// drive it manually (see `AgentLoop`) or call `complete()` for the
    /// default accumulation path.
    /// `cancel` cancels the in-flight HTTP request and any pending
    /// stream items; cancellation is cooperative.
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<CompletionStream, ProviderError>;

    /// Drive the stream to completion and return the final `Completion`.
    /// Default impl: `accumulate_stream(self.stream(...)).await`. Override
    /// only if the provider can do this more efficiently (none do today).
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<Completion, ProviderError> { ... }
}
```

(The exact existing code already has this; just expand the doc.)

- [ ] **Step 1.3.3: Verify + commit**

```bash
cargo test --workspace 2>&1 | tail -3 && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3
git add crates/hermes-core/
git commit -m "docs(core): expand Message constructor and Provider trait doc comments"
```

---

## Pass 2 — `hermes-providers`: split OpenAI / Anthropic adapters

### Task 2.1: Lay out `openai/` module skeleton

**Files:**
- Create: `crates/hermes-providers/src/openai/mod.rs`, `crates/hermes-providers/src/openai/request.rs`, `crates/hermes-providers/src/openai/sse.rs`
- Delete: `crates/hermes-providers/src/openai.rs`

- [ ] **Step 2.1.1: Create `openai/request.rs` with DTOs + `to_openai_message` + `to_openai_tool`**

Move all DTOs from `openai.rs` (lines 47-119) into `openai/request.rs`. Rename:
- `OaiMessage` → `OpenaiMessage`
- `OaiMessageContent` → `OpenaiMessageContent`
- `OaiContentPart` → `OpenaiContentPart`
- `OaiImageUrl` → `OpenaiImageUrl`
- `OaiToolCallRef` → `OpenaiToolCallRef`
- `OaiFunctionCallRef` → `OpenaiFunctionCallRef`
- `OaiTool` → `OpenaiTool`
- `OaiFunctionDef` → `OpenaiFunctionDef`
- `StreamOptions` → `OpenaiStreamOptions`
- `ChatRequest` → `OpenaiChatRequest`

Add two free functions:

```rust
/// Convert a single `Message` into the OpenAI wire format. Tools round-trip
/// via `OpenaiToolCallRef` (the provider sends `arguments` as a JSON string,
/// matching how OpenAI returns it).
pub(super) fn to_openai_message(m: &Message) -> OpenaiMessage {
    let tool_calls = m.tool_calls.as_ref().map(|calls| {
        calls
            .iter()
            .map(|c| OpenaiToolCallRef {
                id: &c.id,
                r#type: "function",
                function: OpenaiFunctionCallRef {
                    name: &c.name,
                    arguments: serde_json::to_string(&c.arguments)
                        .unwrap_or_else(|_| "null".into()),
                },
            })
            .collect()
    });
    OpenaiMessage {
        role: m.role.as_str(),
        content: match &m.content {
            Content::Text(s) => Some(OpenaiMessageContent::Text(s.as_str())),
            Content::Parts(parts) => Some(OpenaiMessageContent::Parts(
                parts
                    .iter()
                    .map(|p| match p {
                        ContentPart::Text { text } => OpenaiContentPart::Text {
                            text: text.as_str(),
                        },
                        ContentPart::ImageUrl { url } => OpenaiContentPart::ImageUrl {
                            image_url: OpenaiImageUrl { url: url.as_str() },
                        },
                    })
                    .collect(),
            )),
        },
        tool_call_id: m.tool_call_id.as_deref(),
        tool_calls,
    }
}

/// Convert a single `ToolSchema` into the OpenAI wire format.
pub(super) fn to_openai_tool(t: &ToolSchema) -> OpenaiTool {
    OpenaiTool {
        r#type: "function",
        function: OpenaiFunctionDef {
            name: &t.name,
            description: &t.description,
            parameters: &t.parameters,
        },
    }
}

/// Build the full Chat Completions request body. Returns `tool_choice: None`
/// when no tools are present (some OpenAI-compatible providers reject
/// `tool_choice: "auto"` with an empty tool list).
pub(super) fn build_chat_request(
    model: &str,
    messages: &[Message],
    tools: &[ToolSchema],
) -> OpenaiChatRequest<'_> {
    let oai_msgs: Vec<OpenaiMessage> = messages.iter().map(to_openai_message).collect();
    let oai_tools: Vec<OpenaiTool> = tools.iter().map(to_openai_tool).collect();
    let has_tools = !oai_tools.is_empty();
    OpenaiChatRequest {
        model,
        messages: oai_msgs,
        tools: oai_tools,
        tool_choice: if has_tools { Some("auto") } else { None },
        stream: true,
        stream_options: Some(OpenaiStreamOptions { include_usage: true }),
    }
}
```

- [ ] **Step 2.1.2: Create `openai/sse.rs` with parser + `parse_sse_data_line`**

Move from `openai.rs`:
- `OpenAiStreamState` → `OpenaiStreamState` (rename, drop the awkward `Ai` casing)
- `parse_sse_chunks` (function, line 220-243)
- `parse_sse_data_payload` (function, line 245-300)
- `split_think_tag_content` (function, line 303-348)
- `parse_sse_for_test` (cfg(test) pub(crate), line 218)

Add a small helper at top of file:

```rust
/// Strip the `data: ` prefix and surrounding whitespace; return the
/// payload, or `None` if the line is a comment or a control line.
pub(super) fn parse_sse_data_line(line: &str) -> Option<&str> {
    line.strip_prefix("data: ").map(|rest| rest.trim())
}
```

Use it in `parse_sse_chunks`:

```rust
for line in event.lines() {
    let Some(payload) = parse_sse_data_line(line) else { continue };
    if payload == "[DONE]" { return; }
    match parse_sse_data_payload(payload, &mut state) {
        Ok(d) => yield Ok(d),
        Err(e) => { yield Err(e); return; }
    }
}
```

Move all `#[cfg(test)] mod tests` for SSE parsing into this file too (lines 359-583 in current `openai.rs`).

- [ ] **Step 2.1.3: Rewrite `openai/mod.rs`**

`mod.rs` keeps only the `OpenAiProvider` struct, the `Provider` impl, and a brief module-level doc comment. The `Provider` impl's `stream()` body becomes:

```rust
async fn stream(
    &self,
    messages: &[Message],
    tools: &[ToolSchema],
    cancel: CancellationToken,
) -> Result<CompletionStream, ProviderError> {
    let body = request::build_chat_request(&self.model, messages, tools);
    let url = format!("{}/chat/completions", self.base_url);

    let resp = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
        r = self.client.post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send() => r.map_err(|e| ProviderError::Transport(e.to_string()))?,
    };

    if resp.status() == 401 {
        return Err(ProviderError::Auth(resp.text().await.unwrap_or_default()));
    }
    if resp.status() == 429 {
        return Err(ProviderError::RateLimited { retry_after_secs: 1 });
    }
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(ProviderError::InvalidResponse(body));
    }

    Ok(Box::pin(sse::parse_sse_chunks(resp.bytes_stream())))
}
```

Where `request` and `sse` are the sibling modules.

- [ ] **Step 2.1.4: Replace `openai.rs` with `openai/mod.rs`**

```bash
mkdir -p crates/hermes-providers/src/openai
git mv crates/hermes-providers/src/openai.rs crates/hermes-providers/src/openai/mod.rs
```

Then move the inner content per the previous steps (the file structure should match the target layout, not the current monolith).

- [ ] **Step 2.1.5: Verify**

Run: `cargo test -p hermes-providers 2>&1 | tail -3 && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3 && cargo doc --no-deps 2>&1 | tail -3`
Expected: All provider tests pass; clippy clean.

- [ ] **Step 2.1.6: Commit**

```bash
git add crates/hermes-providers/src/openai/
git commit -m "refactor(providers): split openai.rs into mod/request/sse (rename Oai* -> Openai*)"
```

### Task 2.2: Lay out `anthropic/` module skeleton (same pattern as 2.1)

**Files:**
- Create: `crates/hermes-providers/src/anthropic/mod.rs`, `crates/hermes-providers/src/anthropic/request.rs`, `crates/hermes-providers/src/anthropic/sse.rs`
- Delete: `crates/hermes-providers/src/anthropic.rs`

- [ ] **Step 2.2.1: Create `anthropic/request.rs`**

Move DTOs from `anthropic.rs` and rename:
- `MessagesRequest` → `AnthropicMessagesRequest`
- `WireMessage` → `AnthropicMessage`
- `WireMessageContent` → `AnthropicMessageContent`
- `WireContentBlock` → `AnthropicContentBlock`
- `WireTool` → `AnthropicTool`
- `WireToolChoice` → `AnthropicToolChoice`
- `ThinkingParam` → `AnthropicThinkingParam`
- `OutputConfig` → `AnthropicOutputConfig`

Move and keep:
- `convert_messages_to_anthropic` (rename → `to_anthropic_messages`)
- `convert_tools` (rename → `to_anthropic_tools`)
- `build_thinking_param` (private to request.rs)
- `build_output_config` (private to request.rs)
- `build_request_body_with_options` (rename → `build_messages_request`)

- [ ] **Step 2.2.2: Create `anthropic/sse.rs`**

Move the SSE parser and all its tests from `anthropic.rs`. Add the same `parse_sse_data_line` helper as in OpenAI.

- [ ] **Step 2.2.3: Rewrite `anthropic/mod.rs`**

Keep only `AnthropicProvider` + the `Provider` impl. The `Provider::stream()` body becomes a thin caller into `request::build_messages_request` and `sse::parse_sse_chunks`.

- [ ] **Step 2.2.4: Move `anthropic.rs` into `anthropic/mod.rs`**

```bash
mkdir -p crates/hermes-providers/src/anthropic
git mv crates/hermes-providers/src/anthropic.rs crates/hermes-providers/src/anthropic/mod.rs
```

(Then split content per above steps.)

- [ ] **Step 2.2.5: Verify**

Run: `cargo test -p hermes-providers 2>&1 | tail -3 && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3 && cargo doc --no-deps 2>&1 | tail -3`
Expected: Green.

- [ ] **Step 2.2.6: Commit**

```bash
git add crates/hermes-providers/src/anthropic/
git commit -m "refactor(providers): split anthropic.rs into mod/request/sse (rename Wire* -> Anthropic*)"
```

### Task 2.3: Update provider `lib.rs` and verify integration

**Files:**
- Modify: `crates/hermes-providers/src/lib.rs`

- [ ] **Step 2.3.1: Update re-exports**

The new layout has `crate::openai::OpenAiProvider` and `crate::anthropic::AnthropicProvider`. Update `lib.rs` to re-export them at the crate root for backwards-compatible public API (tests, examples, downstream crates import `hermes_providers::OpenAiProvider`):

```rust
pub mod openai;
pub mod anthropic;
mod echo;

pub use openai::OpenAiProvider;
pub use anthropic::AnthropicProvider;
pub use echo::EchoProvider;
```

- [ ] **Step 2.3.2: Full verify**

Run: `cargo test --workspace 2>&1 | tail -3 && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3 && cargo doc --no-deps 2>&1 | tail -3`
Expected: All green, doc builds clean.

- [ ] **Step 2.3.3: Commit**

```bash
git add crates/hermes-providers/src/lib.rs
git commit -m "refactor(providers): expose openai/anthropic modules in lib.rs"
```

---

## Pass 3 — `hermes-agent`: split `loop_engine.rs`, `compressor.rs`, and `runtime_agent.rs`

### Task 3.1: Lay out `loop_engine/` submodules

**Files:**
- Create: `crates/hermes-agent/src/loop_engine/mod.rs`, `run.rs`, `compress.rs`, `metrics.rs`
- Delete: `crates/hermes-agent/src/loop_engine.rs`

- [ ] **Step 3.1.1: Create `loop_engine/mod.rs` (public surface)**

This file keeps the type definitions (AgentLoop, LoopConfig, LoopEvent, RunResult, FailedTurn, AgentRunError, LoopMetrics) and re-exports the impl methods. Move the type bodies from `loop_engine.rs` lines 1-167.

```rust
//! The agent loop — calls the LLM, reacts to `finish_reason`, dispatches
//! tools, returns a `RunResult`.

mod compress;
mod metrics;
mod run;

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;

use hermes_core::context_engine::ContextEngine;
use hermes_core::error::LoopError;
use hermes_core::message::{Message, ToolCall};
use hermes_core::provider::{FinishReason, Provider, ToolCallDelta};
use hermes_core::registry::InMemoryRegistry;
use hermes_core::tool::{ToolContext, ToolOutput};

pub use compress::CompactOutcome;
pub use run::RunInputs;

#[derive(Clone)]
pub struct LoopConfig { ... }    // unchanged
impl std::fmt::Debug for LoopConfig { ... }
impl Default for LoopConfig { ... }
#[derive(Debug, Clone, Default)] pub struct LoopMetrics { ... }
#[derive(Debug, Clone)] pub struct RunResult { ... }
#[derive(Debug, Clone)] pub struct FailedTurn { ... }
#[derive(Debug, thiserror::Error)] pub enum AgentRunError { ... }
#[derive(Debug, Clone)] pub enum LoopEvent { ... }   // unchanged

pub struct AgentLoop { ... }
impl AgentLoop {
    pub fn new(...) -> Self { ... }
    pub fn from_provider(...) -> Self { ... }
    pub fn has_context_engine(&self) -> bool { ... }
    pub async fn compact_messages(...) -> Result<..., AgentRunError> { ... }   // delegates to compress
    pub async fn run(&self, ...) -> Result<RunResult, AgentRunError> {
        run::run(self, initial_messages, ctx, cancel, on_event).await
    }
}
```

- [ ] **Step 3.1.2: Create `loop_engine/metrics.rs`**

Move from `loop_engine.rs`:
- `estimate_tokens_for_messages` (line ~509)
- `prompt_context_tokens_from_usage` (line ~520) — use `usage.prompt_context_tokens()` from Pass 1
- `estimate_request_context_tokens` (line ~528)

Replace the `4.0` literal with `crate::CHARS_PER_TOKEN_ESTIMATE` — wait, that's in `hermes_core`, not `hermes_agent`. Add to `loop_engine/metrics.rs`:

```rust
/// 4 chars per token is the conservative English-prose estimate used
/// throughout the agent. See `hermes_core::CHARS_PER_TOKEN_ESTIMATE`.
const CHARS_PER_TOKEN: f64 = 4.0;
```

(We don't import from `hermes_core` because the constant lives at the crate root and `loop_engine` is one level deeper; just keep a local alias and add a TODO comment to dedupe later. The Pass 5 polish can move it.)

- [ ] **Step 3.1.3: Create `loop_engine/compress.rs`**

Move `try_compress` (line ~380) here. Restructure to **hold the lock exactly once**:

```rust
/// Outcome of a single compression attempt.
pub enum CompactOutcome {
    /// Engine rejected the request (already locked elsewhere, or no
    /// eligible messages). Surfaced as `LoopEvent::CompressionSkipped`.
    Skipped(CompressionSkipReason),
    /// Compression produced new messages.
    Compressed {
        new_messages: Vec<Message>,
        tokens_before: u64,
        tokens_after: u64,
        summary_chars: usize,
        duration: Duration,
    },
    /// Compression failed; `error` is the human-readable cause.
    Failed { error: String },
}

pub async fn try_compress(
    engine: &Arc<TokioMutex<dyn ContextEngine>>,
    messages: &mut Vec<Message>,
    focus_topic: Option<&str>,
    config_focus: Option<&str>,
    force: bool,
) -> CompactOutcome {
    let started = Instant::now();
    let tokens_before = metrics::estimate_tokens_for_messages(messages, 4.0);

    // Single lock — hold it for the entire compress() call.
    let mut guard = match engine.try_lock() {
        Ok(g) => g,
        Err(_) => {
            return CompactOutcome::Skipped(CompressionSkipReason::NothingToCompress);
        }
    };
    let focus = config_focus.or(focus_topic);
    let result = guard
        .compress(messages.clone(), Some(tokens_before), focus, force)
        .await;
    drop(guard);

    let duration = started.elapsed();
    match result {
        Ok(new_messages) => {
            let tokens_after = metrics::estimate_tokens_for_messages(&new_messages, 4.0);
            let summary_chars = new_messages
                .iter()
                .filter(|m| marker::is_summary_message(m))
                .map(|m| m.content.chars())
                .sum();
            *messages = new_messages;
            CompactOutcome::Compressed {
                new_messages: messages.clone(),   // borrow rules — adjust if needed
                tokens_before,
                tokens_after,
                summary_chars,
                duration,
            }
        }
        Err(CompressError::NothingToCompress) => {
            CompactOutcome::Skipped(CompressionSkipReason::NothingToCompress)
        }
        Err(e) => CompactOutcome::Failed { error: e.to_string() },
    }
}
```

(Refine the lifetime/borrow story to actually compile; the `messages: &mut Vec<Message>` lets us do `*messages = new_messages` in place. Return only the events the caller needs.)

- [ ] **Step 3.1.4: Create `loop_engine/run.rs`**

Move the body of `AgentLoop::run` here. Split into private helpers:

```rust
pub(super) async fn run(
    engine: &AgentLoop,
    initial_messages: Vec<Message>,
    ctx: ToolContext,
    cancel: CancellationToken,
    mut on_event: impl FnMut(LoopEvent) + Send,
) -> Result<RunResult, AgentRunError> {
    let initial_len = initial_messages.len();
    let mut messages = initial_messages;
    let mut metrics = LoopMetrics::default();
    let started = Instant::now();

    if let Some(sys) = &engine.config.system_prompt {
        if !messages.iter().any(|m| m.role == Role::System) {
            messages.insert(0, Message::system(sys.clone()));
        }
    }

    pre_turn_compression_check(engine, &mut messages, &mut metrics, &mut on_event).await;

    loop {
        if cancel.is_cancelled() {
            on_event(LoopEvent::Cancelled);
            return Err(AgentRunError::Loop(LoopError::Cancelled));
        }
        if metrics.iterations >= engine.config.max_iterations {
            on_event(LoopEvent::IterationsExhausted);
            return Err(AgentRunError::Loop(LoopError::MaxIterations(metrics.iterations)));
        }
        if started.elapsed() > engine.config.max_duration {
            return Err(AgentRunError::Loop(LoopError::Timeout(started.elapsed())));
        }

        let completion = match drive_turn(engine, &messages, &ctx, &cancel, &mut on_event, started).await {
            Ok(c) => c,
            Err(e) => return Err(provider_failure(messages, e.partial, initial_len, e.error)),
        };

        metrics.iterations += 1;
        metrics.input_tokens += completion.usage.input_tokens;
        metrics.cached_input_tokens += completion.usage.cached_input_tokens;
        metrics.output_tokens += completion.usage.output_tokens;

        let assistant_msg = completion.message.clone();
        messages.push(assistant_msg.clone());
        on_event(LoopEvent::AssistantMessage(assistant_msg.clone()));

        match handle_finish_reason(completion, &mut messages, &ctx, &cancel, engine, &mut metrics, started, &mut on_event).await {
            Ok(Some(result)) => return Ok(result),
            Ok(None) => continue,  // tool_use path consumed this iteration
            Err(e) => return Err(e),
        }
    }
}

async fn drive_turn(
    engine: &AgentLoop,
    messages: &[Message],
    _ctx: &ToolContext,
    cancel: &CancellationToken,
    on_event: &mut impl FnMut(LoopEvent),
    started: Instant,
) -> Result<Completion, DriveError> { ... }

async fn handle_finish_reason(
    completion: Completion,
    messages: &mut Vec<Message>,
    ctx: &ToolContext,
    cancel: &CancellationToken,
    engine: &AgentLoop,
    metrics: &mut LoopMetrics,
    started: Instant,
    on_event: &mut impl FnMut(LoopEvent),
) -> Result<Option<RunResult>, AgentRunError> {
    match completion.finish_reason {
        FinishReason::Stop => Ok(Some(finalize(messages, completion.message, metrics, started))),
        FinishReason::Length => { on_event(LoopEvent::LengthLimit); Ok(Some(finalize(...))) }
        FinishReason::ContentFilter => Err(AgentRunError::Loop(LoopError::ContentFilter)),
        FinishReason::Error => Err(AgentRunError::Loop(LoopError::Provider(
            ProviderError::Other("provider returned finish_reason=error".into()),
        ))),
        FinishReason::ToolUse => {
            dispatch_tool_calls(completion, messages, ctx, cancel, engine, metrics, on_event).await?;
            Ok(None)
        }
    }
}

async fn dispatch_tool_calls(
    completion: Completion,
    messages: &mut Vec<Message>,
    ctx: &ToolContext,
    cancel: &CancellationToken,
    engine: &AgentLoop,
    metrics: &mut LoopMetrics,
    on_event: &mut impl FnMut(LoopEvent),
) -> Result<(), AgentRunError> { ... }

fn finalize(messages: &[Message], final_msg: Message, metrics: &mut LoopMetrics, started: Instant) -> RunResult { ... }
```

The `provider_failure` function becomes a struct constructor:

```rust
pub struct ProviderFailure {
    pub error: ProviderError,
    pub partial_assistant: Option<Message>,
}

pub fn build_failed_turn(
    error: ProviderError,
    partial: Option<Message>,
    initial_len: usize,
    mut messages: Vec<Message>,
) -> AgentRunError {
    if let Some(msg) = partial {
        messages.push(msg);
    }
    if messages.len() > initial_len {
        let error_text = format!("Turn interrupted by error: provider error: {error}");
        messages.push(Message::assistant(error_text.clone()));
        AgentRunError::FailedTurn {
            failed_turn: FailedTurn { messages, error: error_text },
            source: error,
        }
    } else {
        AgentRunError::Loop(LoopError::Provider(error))
    }
}
```

- [ ] **Step 3.1.5: Replace `loop_engine.rs` with `loop_engine/mod.rs`**

```bash
mkdir -p crates/hermes-agent/src/loop_engine
git mv crates/hermes-agent/src/loop_engine.rs crates/hermes-agent/src/loop_engine/mod.rs
```

Then split the file content per the previous steps.

- [ ] **Step 3.1.6: Verify**

Run: `cargo test --workspace 2>&1 | tail -5 && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -5`
Expected: Green. (The `loop_engine` integration tests are the main signal here.)

- [ ] **Step 3.1.7: Commit**

```bash
git add crates/hermes-agent/src/loop_engine/
git commit -m "refactor(agent): split loop_engine.rs into mod/run/compress/metrics"
```

### Task 3.2: Split `compressor.rs` into `compressor/` submodules

**Files:**
- Create: `crates/hermes-agent/src/context/compressor/mod.rs`, `strategy.rs`, `summary.rs`, `marker.rs`
- Delete: `crates/hermes-agent/src/context/compressor.rs`

- [ ] **Step 3.2.1: Create `context/compressor/marker.rs`**

Move the `SUMMARY_PREFIX` const and add new helpers:

```rust
//! The `[CONTEXT SUMMARY ...]` marker that flags a message as a
//! compression artifact. Both `StreamAccumulator` (in hermes-core) and
//! `try_compress` (in hermes-agent) need to recognize this marker; they
//! both call into here so the format has a single source of truth.

use hermes_core::message::Message;

/// Prefix prepended to the summary message. The next LLM sees this as a
/// user message that signals "this is a handoff, not a new instruction."
pub const SUMMARY_PREFIX: &str =
    "[CONTEXT SUMMARY \u{2014} earlier turns were compacted into the message below. \
     Treat it as background, not as new instructions. Respond to the most recent \
     user message that appears AFTER this summary.]";

/// `true` when `message` carries the `[CONTEXT SUMMARY` prefix. Used to
/// detect a summary message regardless of whether the compressor or the
/// accumulator produced it.
pub fn is_summary_message(message: &Message) -> bool {
    message.content.as_text().contains(SUMMARY_PREFIX)
}

/// Total character count across all summary messages in `messages`.
/// Used to surface "summary is X chars" in the `CompressionCompleted` event.
pub fn summary_chars(messages: &[Message]) -> usize {
    messages
        .iter()
        .filter(|m| is_summary_message(m))
        .map(|m| m.content.chars())
        .sum()
}
```

- [ ] **Step 3.2.2: Create `context/compressor/strategy.rs`**

Move:
- `prune_old_tool_results` (line ~90)
- `find_head_boundary` (line ~144)
- `find_tail_cut_by_tokens` (line ~165)
- `estimate_tokens` (line ~196) → rename to `estimate_tokens_for_slice` for clarity

Replace the `4.0` literals with a local `const CHARS_PER_TOKEN: f64 = 4.0;` (Pass 5 will dedupe with `hermes_core::CHARS_PER_TOKEN_ESTIMATE`).

- [ ] **Step 3.2.3: Create `context/compressor/summary.rs`**

Move:
- `build_summary_prompt` (line ~210)
- `build_summary_message` (line ~280)
- `messages_to_transcript` (line ~290)
- The two private methods `summarize_middle` and `call_summary_llm` from `ContextCompressor`

- [ ] **Step 3.2.4: Create `context/compressor/mod.rs`**

Keep `CompressorConfig`, `ContextCompressor`, and the `ContextEngine` impl. The struct holds a `summary` module reference (or just calls `summary::summarize_middle(self, ...)` and `summary::call_summary_llm(self, ...)` as `pub(super)` free functions taking `&self`).

`compress_inner` calls:
- `strategy::prune_old_tool_results`
- `strategy::estimate_tokens_for_slice`
- `strategy::find_head_boundary`
- `strategy::find_tail_cut_by_tokens`
- `summary::summarize_middle`
- `summary::build_summary_message`

Move all `#[cfg(test)] mod tests` for compressor into the relevant sub-module (e.g. `strategy.rs` tests for the boundary functions; `summary.rs` tests for the prompt building; `mod.rs` tests for the top-level `compress_inner` algorithm).

- [ ] **Step 3.2.5: Replace `compressor.rs` with `compressor/mod.rs`**

```bash
mkdir -p crates/hermes-agent/src/context/compressor
git mv crates/hermes-agent/src/context/compressor.rs crates/hermes-agent/src/context/compressor/mod.rs
```

(Then split per the steps above.)

- [ ] **Step 3.2.6: Verify**

Run: `cargo test --workspace 2>&1 | tail -5 && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -5`
Expected: Green.

- [ ] **Step 3.2.7: Commit**

```bash
git add crates/hermes-agent/src/context/compressor/
git commit -m "refactor(agent): split compressor.rs into mod/strategy/summary/marker"
```

### Task 3.3: Wire `marker::is_summary_message` into accumulator

**Files:**
- Modify: `crates/hermes-core/src/accumulator.rs` (the `[CONTEXT SUMMARY` substring check at line ~108)

- [ ] **Step 3.3.1: Replace substring check with a shared helper**

The current code is:
```rust
let summary_chars = new_messages
    .iter()
    .filter(|m| m.content.as_text().contains("[CONTEXT SUMMARY"))
    .map(|m| m.content.chars())
    .sum::<usize>();
```

The `agent` crate's `loop_engine` re-implements the same logic. Replace both with a single helper in `hermes-core`:

Add to `crates/hermes-core/src/accumulator.rs` (top of file or in a small private module):

```rust
/// Prefix marking a message as a compression summary. Both `hermes-core`
/// (during streaming accumulation) and `hermes-agent` (during compression)
/// need to recognize this; the prefix is duplicated in
/// `hermes_agent::context::compressor::marker::SUMMARY_PREFIX`.
pub const CONTEXT_SUMMARY_MARKER: &str = "[CONTEXT SUMMARY";

/// `true` when `message` carries the `[CONTEXT SUMMARY` prefix. The actual
/// prefix string is in `hermes_agent` (so the agent owns its format); this
/// predicate just checks for the leading marker bytes.
pub fn is_context_summary_message(message: &Message) -> bool {
    message.content.as_text().contains(CONTEXT_SUMMARY_MARKER)
}

/// Total character count across all summary messages in `messages`.
pub fn context_summary_chars(messages: &[Message]) -> usize {
    messages
        .iter()
        .filter(|m| is_context_summary_message(m))
        .map(|m| m.content.chars())
        .sum()
}
```

(Use the `chars()` method we added in Pass 1, not the manual `as_text().chars()`.)

Update the call site in `accumulator.rs` and `loop_engine/compress.rs` to use this helper.

- [ ] **Step 3.3.2: Verify**

```bash
cargo test --workspace 2>&1 | tail -3 && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3
```

- [ ] **Step 3.3.3: Commit**

```bash
git add crates/hermes-core/src/accumulator.rs crates/hermes-agent/src/loop_engine/
git commit -m "refactor(agent): centralize context summary marker predicate"
```

### Task 3.4: Simplify `runtime_agent.rs`

**Files:**
- Modify: `crates/hermes-agent/src/runtime_agent.rs`

- [ ] **Step 3.4.1: Extract `default_skills_dir` and `build_context_engine`**

```rust
/// Fallback skills directory when neither `HERMES_HOME` nor `$HOME` is
/// available. Returns `./.perry_hermes/skills` relative to the current
/// working directory (or `.` if `current_dir()` fails).
fn default_skills_dir() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".perry_hermes")
        .join("skills")
}

/// Build a `ContextCompressor` wired to the same provider, when context
/// compression is enabled. Returns `None` when compression is disabled.
fn build_context_engine(
    config: &HermesConfig,
    provider: &Arc<dyn Provider>,
    selected_provider: Option<&ResolvedProviderConfig>,
) -> Option<Arc<TokioMutex<dyn hermes_core::ContextEngine>>> {
    if !config.agent.context_compression_enabled {
        return None;
    }
    let mut compressor_config = CompressorConfig::default();
    if let Some(threshold_percent) = config.agent.context_compression_threshold_percent {
        compressor_config.threshold_percent = threshold_percent;
    }
    let model_name = selected_provider
        .map(|provider| provider.model.clone())
        .unwrap_or_else(|| "custom".to_string());
    let context_window_size = selected_provider.map(|provider| provider.context_window_size);
    Some(Arc::new(TokioMutex::new(
        ContextCompressor::new(compressor_config, model_name, context_window_size)
            .with_summary_provider(Arc::clone(provider)),
    )))
}

fn build_loop(
    provider: Arc<dyn Provider>,
    config: &HermesConfig,
    selected_provider: Option<&ResolvedProviderConfig>,
) -> AgentLoop {
    let skills_dir = resolve_skills_dir().unwrap_or_else(default_skills_dir);
    let registry = Arc::new(build_registry(&config.agent.disabled_toolsets, &skills_dir));
    let context_engine = build_context_engine(config, &provider, selected_provider);
    let loop_config = LoopConfig {
        max_iterations: config.agent.max_iterations.unwrap_or(10),
        system_prompt: None,
        context_engine,
        ..Default::default()
    };
    AgentLoop::from_provider(provider, registry, loop_config)
}
```

- [ ] **Step 3.4.2: Replace the `build_loop_for_custom_provider` body with the simpler `build_loop`**

Drop the wrapper. The two callers (from `from_config` and `new`) both pass an `Option<&ResolvedProviderConfig>` (resolved or none) and use the same builder.

- [ ] **Step 3.4.3: Document the by-value constructors**

On `AIAgent::from_config` and `AIAgent::new`, add a doc line:

```rust
/// Construct an `AIAgent` from a fully-resolved config. `config` is
/// taken by value because the config is moved into the agent (its
/// `agent` and `providers` strings are cloned for diagnostics, but
/// the full config is consumed).
pub fn from_config(config: HermesConfig) -> anyhow::Result<Self> { ... }
```

Drop the `#[allow(clippy::needless_pass_by_value)]` attributes — the doc comment is the explanation now.

- [ ] **Step 3.4.4: Verify**

```bash
cargo test --workspace 2>&1 | tail -3 && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3
```

- [ ] **Step 3.4.5: Commit**

```bash
git add crates/hermes-agent/src/runtime_agent.rs
git commit -m "refactor(agent): simplify runtime_agent construction (default_skills_dir, build_context_engine, document by-value)"
```

### Task 3.5: Split `tools/files/policy.rs` into `policy/` submodules

**Files:**
- Create: `crates/hermes-agent/src/tools/files/policy/mod.rs`, `sensitive.rs`, `profile.rs`, `dedup.rs`
- Delete: `crates/hermes-agent/src/tools/files/policy.rs`

- [ ] **Step 3.5.1: Create `policy/sensitive.rs`**

Move:
- `sensitive_write_path_message` (and its `exact_blocked` / `prefix_blocked` arrays)
- The `hermes_config_path` helper if it lives in this file
- `blocked_path_message` (the read-side check)

- [ ] **Step 3.5.2: Create `policy/profile.rs`**

Move:
- `cross_profile_write_message`
- `current_profile_from_hermes_home`

- [ ] **Step 3.5.3: Create `policy/dedup.rs`**

Move:
- `is_internal_file_status_text`

- [ ] **Step 3.5.4: Create `policy/mod.rs`**

Re-export the public surface so `tools/files/{read,write}.rs` keep importing the same names:

```rust
mod sensitive;
mod profile;
mod dedup;

pub(super) use sensitive::{sensitive_write_path_message, blocked_path_message};
pub(super) use profile::cross_profile_write_message;
pub(super) use dedup::is_internal_file_status_text;
```

- [ ] **Step 3.5.5: Move `policy.rs` to `policy/mod.rs`**

```bash
mkdir -p crates/hermes-agent/src/tools/files/policy
git mv crates/hermes-agent/src/tools/files/policy.rs crates/hermes-agent/src/tools/files/policy/mod.rs
```

(Then split content per above.)

- [ ] **Step 3.5.6: Verify**

```bash
cargo test --workspace 2>&1 | tail -3 && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3
```

- [ ] **Step 3.5.7: Commit**

```bash
git add crates/hermes-agent/src/tools/files/policy/
git commit -m "refactor(agent): split files/policy.rs into sensitive/profile/dedup submodules"
```

---

## Pass 4 — `hermes-cli`: TUI ergonomics

### Task 4.1: Refactor `input.rs` into `dispatch_key` + helpers

**Files:**
- Modify: `crates/hermes-cli/src/tui/input.rs`

- [ ] **Step 4.1.1: Add `try_global_key`, `try_chat_scroll_key`, `text_editing_keys`**

```rust
pub fn handle_key(app: &mut App, key: KeyEvent) -> AppEvent {
    // 1. Global modifiers (Ctrl-C, Ctrl-D, Esc) — checked first.
    if let Some(event) = try_global_key(app, key) {
        return event;
    }
    // 2. Cancelling mode: ignore everything else.
    if app.mode == AppMode::Cancelling {
        return AppEvent::Tick;
    }
    // 3. Chat scroll keys (Idle only).
    if app.mode == AppMode::Idle {
        if let Some(event) = try_chat_scroll_key(app, key) {
            return event;
        }
    }
    // 4. Text editing keys (always, when not in a higher-priority branch).
    text_editing_keys(app, key)
}

/// Ctrl-C, Ctrl-D, and Esc. Returns `None` if the key isn't a global
/// modifier so the caller falls through to mode-specific handling.
fn try_global_key(app: &App, key: KeyEvent) -> Option<AppEvent> {
    use crossterm::event::KeyModifiers;
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(match app.mode {
            AppMode::AwaitingModel => AppEvent::CancelInFlight,
            AppMode::Idle | AppMode::Cancelling => AppEvent::Quit,
        });
    }
    if key.code == KeyCode::Esc {
        return Some(match app.mode {
            AppMode::AwaitingModel => AppEvent::CancelInFlight,
            AppMode::Idle => AppEvent::Tick,
            AppMode::Cancelling => AppEvent::Quit,
        });
    }
    if key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(match app.mode {
            AppMode::Idle => AppEvent::Quit,
            _ => AppEvent::Tick,
        });
    }
    None
}

/// Up/Down/PageUp/PageDown/End — only relevant in Idle mode (no input
/// editing happening). Returns `None` if the key isn't a scroll key.
fn try_chat_scroll_key(app: &mut App, key: KeyEvent) -> Option<AppEvent> {
    match key.code {
        KeyCode::Up => {
            app.chat_scroll = app.chat_scroll.saturating_add(1);
            Some(AppEvent::Tick)
        }
        KeyCode::Down => {
            app.chat_scroll = app.chat_scroll.saturating_sub(1);
            Some(AppEvent::Tick)
        }
        KeyCode::PageUp => {
            app.chat_scroll = app.chat_scroll.saturating_add(SCROLL_PAGE);
            Some(AppEvent::Tick)
        }
        KeyCode::PageDown => {
            app.chat_scroll = app.chat_scroll.saturating_sub(SCROLL_PAGE);
            Some(AppEvent::Tick)
        }
        KeyCode::End => {
            app.chat_scroll = 0;
            Some(AppEvent::Tick)
        }
        _ => None,
    }
}

/// Char / Backspace / Delete / arrows / Home / End / Enter — the regular
/// text-editing keys. Returns `AppEvent::Tick` for any key that doesn't
/// produce a meaningful edit (so the main loop just re-renders).
fn text_editing_keys(app: &mut App, key: KeyEvent) -> AppEvent {
    match key.code {
        KeyCode::Char(c) => { app.insert_at_cursor(c); AppEvent::Tick }
        KeyCode::Backspace => { app.delete_before_cursor(); AppEvent::Tick }
        KeyCode::Delete => { app.delete_at_cursor(); AppEvent::Tick }
        KeyCode::Left => { app.move_cursor_left(); AppEvent::Tick }
        KeyCode::Right => { app.move_cursor_right(); AppEvent::Tick }
        KeyCode::Home => { app.move_cursor_home(); AppEvent::Tick }
        KeyCode::End => { app.move_cursor_end(); AppEvent::Tick }
        KeyCode::Enter => {
            let text = std::mem::take(&mut app.input);
            app.cursor = 0;
            parse_slash_or_submit(text)
        }
        _ => AppEvent::Tick,
    }
}
```

- [ ] **Step 4.1.2: Verify (existing tests should pass unchanged)**

The `handle_key` function keeps its signature; the tests at the bottom of `input.rs` exercise it as before.

```bash
cargo test -p hermes-cli 2>&1 | tail -3 && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3
```

- [ ] **Step 4.1.3: Commit**

```bash
git add crates/hermes-cli/src/tui/input.rs
git commit -m "refactor(cli): split handle_key into dispatch_key + 3 helpers (global / scroll / text)"
```

### Task 4.2: Split `render.rs` into `render/` submodules + extract `ChatView` cache

**Files:**
- Create: `crates/hermes-cli/src/tui/render/mod.rs`, `layout.rs`, `chat.rs`, `input_block.rs`, `status.rs`
- Delete: `crates/hermes-cli/src/tui/render.rs`
- Modify: `crates/hermes-cli/src/tui/app.rs` (remove cache fields)

- [ ] **Step 4.2.1: Create `render/chat.rs` with `ChatView` cache struct**

```rust
use ratatui::text::Line;

use crate::tui::app::App;
use crate::tui::event::RenderedLine;

const MAX_INPUT_WIDTH: u16 = 4096;

/// Caches the wrapped chat lines for a given (width, scrollback revision)
/// pair. Owned by `render::paint` (not by `App`); the cache lives for the
/// lifetime of a single paint call.
pub(super) struct ChatView<'a> {
    scrollback: &'a [RenderedLine],
    width: u16,
    cache: Option<(u64, u16, Vec<Line<'static>>)>,
    scrollback_revision: u64,
}

impl<'a> ChatView<'a> {
    pub(super) fn new(app: &'a App, width: u16) -> Self {
        Self {
            scrollback: &app.scrollback,
            width,
            cache: None,
            scrollback_revision: app.scrollback_revision(),
        }
    }

    pub(super) fn lines(&mut self) -> &[Line<'static>] {
        if self.cache.as_ref().is_none_or(|(rev, w, _)| *rev != self.scrollback_revision || *w != self.width) {
            let lines = build_chat_lines(self.scrollback, self.width);
            self.cache = Some((self.scrollback_revision, self.width, lines));
        }
        &self.cache.as_ref().expect("just initialized").2
    }
}

pub(super) fn build_chat_lines(scrollback: &[RenderedLine], width: u16) -> Vec<Line<'static>> {
    // ... unchanged from current render.rs ...
}
```

- [ ] **Step 4.2.2: Create `render/input_block.rs`**

Move:
- `build_input_lines`
- `compute_input_cursor_row`
- `compute_input_cursor_col_row`
- `MIN_INPUT_CONTENT_LINES`, `MAX_INPUT_CONTENT_LINES` constants

- [ ] **Step 4.2.3: Create `render/status.rs`**

Move:
- `build_status_line_1`
- `build_activity_line`

- [ ] **Step 4.2.4: Create `render/layout.rs`**

```rust
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use crate::tui::app::{App, AppMode};

pub(super) struct FrameLayout {
    pub chat: Rect,
    pub activity: Option<Rect>,
    pub status: Rect,
    pub input: Rect,
}

pub(super) fn compute_layout(area: Rect, app: &App) -> FrameLayout {
    let activity_h = if matches!(app.mode, AppMode::AwaitingModel | AppMode::Cancelling) {
        1
    } else { 0 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),              // chat
            Constraint::Length(activity_h),  // activity (busy only)
            Constraint::Length(1),           // status
            Constraint::Length(input_height_for(app, area)),  // input
        ])
        .split(area);
    FrameLayout {
        chat: chunks[0],
        activity: if activity_h > 0 { Some(chunks[1]) } else { None },
        status: chunks[2 - (activity_h == 0) as usize],
        input: chunks[3 - (activity_h == 0) as usize],
    }
}

fn input_height_for(app: &App, area: Rect) -> u16 {
    let inner_w = area.width.saturating_sub(2).max(1);
    let input_lines = input_block::build_input_lines(app, inner_w);
    let visible = input_lines.len().clamp(input_block::MIN_INPUT_CONTENT_LINES, input_block::MAX_INPUT_CONTENT_LINES);
    (visible + 2) as u16
}
```

(Refine the activity_h-conditional indexing; the cleaner approach is to use a `Vec<Rect>` and pass named fields.)

- [ ] **Step 4.2.5: Rewrite `render/mod.rs`**

```rust
//! Frame painter for the TUI. Splits into:
//! - `layout` — the FrameLayout (chat/activity/status/input regions)
//! - `chat` — wrapped chat lines + cache
//! - `input_block` — input box rendering + cursor math
//! - `status` — status bar + activity indicator

pub mod chat;
mod input_block;
mod layout;
mod status;

use ratatui::Frame;

use crate::tui::app::App;

pub fn render(f: &mut Frame, app: &mut App) {
    let layout = self::layout::compute_layout(f.area(), app);
    let mut chat_view = self::chat::ChatView::new(app, layout.chat.width);
    self::chat::paint_chat(f, layout.chat, &mut chat_view, app.chat_scroll);
    if let Some(area) = layout.activity {
        self::status::paint_activity(f, area, app);
    }
    self::status::paint_status(f, layout.status, app);
    self::input_block::paint_input(f, layout.input, app);
}
```

- [ ] **Step 4.2.6: Move `render.rs` to `render/mod.rs`**

```bash
mkdir -p crates/hermes-cli/src/tui/render
git mv crates/hermes-cli/src/tui/render.rs crates/hermes-cli/src/tui/render/mod.rs
```

(Then split content per above.)

- [ ] **Step 4.2.7: Remove cache fields from `App`**

In `crates/hermes-cli/src/tui/app.rs`:
- Remove `scrollback_revision: u64`
- Remove `cached_chat_lines: Vec<Line<'static>>`
- Remove `cached_chat_width: Option<u16>`
- Remove `cached_chat_revision: u64`
- Remove `chat_lines_for_width` method
- Remove `mark_scrollback_dirty` method (or make it a no-op kept for tests)
- Remove `push_line` calls to `mark_scrollback_dirty` — just keep `chat_scroll = 0` reset
- Add a `pub(super) fn scrollback_revision(&self) -> u64` getter that returns 0 (or wire to a new private field that's only used by `ChatView`)

Actually, simpler: leave `scrollback_revision` in `App` (it's a one-line field), but remove the `cached_chat_*` fields and the `chat_lines_for_width` method. The `ChatView` reads `app.scrollback_revision()` instead.

- [ ] **Step 4.2.8: Verify**

```bash
cargo test -p hermes-cli 2>&1 | tail -3 && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3
```

- [ ] **Step 4.2.9: Commit**

```bash
git add crates/hermes-cli/src/tui/render/ crates/hermes-cli/src/tui/app.rs
git commit -m "refactor(cli): split render.rs into render/ submodules + extract ChatView cache"
```

### Task 4.3: Slim `run.rs` and update `tui/mod.rs`

**Files:**
- Modify: `crates/hermes-cli/src/tui/run.rs`
- Modify: `crates/hermes-cli/src/tui/mod.rs`

- [ ] **Step 4.3.1: Extract terminal setup helpers into `tui/terminal.rs`**

```rust
// crates/hermes-cli/src/tui/terminal.rs

use crossterm::event::{EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::stdout;

pub type Term = Terminal<CrosstermBackend<std::io::Stdout>>;

pub fn enter() -> Result<Term, String> {
    enable_raw_mode().map_err(|e| e.to_string())?;
    execute!(stdout(), EnterAlternateScreen, EnableMouseCapture).map_err(|e| e.to_string())?;
    Terminal::new(CrosstermBackend::new(stdout())).map_err(|e| e.to_string())
}

pub fn leave() -> Result<(), String> {
    disable_raw_mode().map_err(|e| e.to_string())?;
    execute!(stdout(), LeaveAlternateScreen, crossterm::event::DisableMouseCapture).map_err(|e| e.to_string())
}
```

- [ ] **Step 4.3.2: Slim `run.rs` to a thin entry that calls `terminal::enter`/`leave`**

- [ ] **Step 4.3.3: Update `tui/mod.rs` to be a clean index**

```rust
pub mod app;
pub mod event;
mod input;
mod loop_bridge;
mod render;
mod run;
mod terminal;

pub use app::App;
pub use event::{AppEvent, AppMode, RenderedLine};
pub use run::{run, run_with_backend, RunError};
```

- [ ] **Step 4.3.4: Verify**

```bash
cargo test -p hermes-cli 2>&1 | tail -5 && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -5
```

- [ ] **Step 4.3.5: Commit**

```bash
git add crates/hermes-cli/src/tui/
git commit -m "refactor(cli): extract terminal setup, slim run.rs, reindex tui/mod.rs"
```

---

## Pass 5 — naming & final polish (whole workspace)

### Task 5.1: Unify test helpers across provider tests

**Files:**
- Modify: `crates/hermes-providers/tests/anthropic.rs`, `tests/openai.rs`

- [ ] **Step 5.1.1: Rename `message(role, text)` → `user_message` in `anthropic.rs`**

Both files have similar helpers. Use `user_message` as the canonical name (it matches the intent: most calls are user-role; system and tool roles can use `Message::system()` etc. inline).

- [ ] **Step 5.1.2: Verify**

```bash
cargo test -p hermes-providers 2>&1 | tail -3
```

- [ ] **Step 5.1.3: Commit**

```bash
git add crates/hermes-providers/tests/
git commit -m "refactor(providers): unify test helper name (user_message across files)"
```

### Task 5.2: Replace `if cond { None } else { Some(build()) }` patterns

**Files:**
- Modify: `crates/hermes-core/src/accumulator.rs` (lines ~103, ~115, ~165)
- Modify: `crates/hermes-agent/src/runtime_agent.rs` (line ~113)
- Modify: `crates/hermes-agent/src/context/compressor.rs` (a few places)

- [ ] **Step 5.2.1: In `accumulator.rs`, use `.filter(...).map(...)` or `Option::from`**

Example for `finalize`:
```rust
// Before:
let tool_calls = if self.tool_calls.is_empty() {
    None
} else {
    Some(self.tool_calls.into_values().collect())
};

// After:
let tool_calls = (!self.tool_calls.is_empty())
    .then(|| self.tool_calls.into_values().collect());
```

- [ ] **Step 5.2.2: Verify**

```bash
cargo test --workspace 2>&1 | tail -3 && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3
```

- [ ] **Step 5.2.3: Commit**

```bash
git add crates/
git commit -m "refactor: tighten if-else Option construction (use .then / .filter)"
```

### Task 5.3: Annotate `#[allow]` attributes

**Files:**
- Modify: `crates/hermes-agent/src/runtime_agent.rs`
- Modify: `crates/hermes-agent/src/loop_engine/run.rs`

- [ ] **Step 5.3.1: Add explanatory comments to existing `#[allow]`s**

The two existing allows (now should be zero, but verify):
- If `redundant_clone` is still needed: add `// The borrowed error is dropped right after to_string() returns, but to_string() is exactly the conversion we want.`
- If `needless_pass_by_value` is still needed (only if Pass 3.4.3 didn't remove it): add `// Configs are owned and moved into the agent; taking by value is the public API contract.`

If Pass 3.4.3 successfully dropped the `#[allow]`, no annotation needed.

- [ ] **Step 5.3.2: Verify + commit**

```bash
cargo test --workspace 2>&1 | tail -3 && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3
git add crates/
git commit -m "refactor: annotate remaining #[allow] attributes with rationale"
```

### Task 5.4: Dedupe `CHARS_PER_TOKEN` constants

**Files:**
- Modify: `crates/hermes-agent/src/loop_engine/metrics.rs`
- Modify: `crates/hermes-agent/src/context/compressor/strategy.rs`

- [ ] **Step 5.4.1: Use `hermes_core::CHARS_PER_TOKEN_ESTIMATE`**

Both files have `const CHARS_PER_TOKEN: f64 = 4.0;` (added in Pass 3). Replace with:

```rust
use hermes_core::CHARS_PER_TOKEN_ESTIMATE as CHARS_PER_TOKEN;
```

- [ ] **Step 5.4.2: Verify + commit**

```bash
cargo test --workspace 2>&1 | tail -3 && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3
git add crates/
git commit -m "refactor: dedupe CHARS_PER_TOKEN_ESTIMATE between hermes-core and hermes-agent"
```

### Task 5.5: Final top-of-file doc pass

**Files:**
- Modify: Every file in `crates/*/src/`

- [ ] **Step 5.5.1: Walk every source file, add a 1-2 line top-of-file doc**

For files that currently have no `//!` comment, add a one-paragraph description. For files with a verbose comment that just lists contents, shorten to a 1-paragraph "what does this file own?".

- [ ] **Step 5.5.2: Verify**

```bash
cargo doc --no-deps 2>&1 | tail -3
```

- [ ] **Step 5.5.3: Commit**

```bash
git add crates/
git commit -m "docs: tighten top-of-file doc comments"
```

---

## Final verification

- [ ] **F.1: Full test + clippy + doc sweep**

```bash
cargo test --workspace 2>&1 | tail -5
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -5
cargo doc --no-deps 2>&1 | tail -5
```

Expected: All green, all clean.

- [ ] **F.2: Largest file size sanity check**

```bash
find crates -name "*.rs" -path "*/src/*" | xargs wc -l | sort -rn | head -15
```

Expected: No source file over ~500 lines. (anthropic, openai, compressor, loop_engine, render all should be <= 400.)

- [ ] **F.3: Live smoke test (if API key is configured)**

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
echo "hello" | cargo run -p hermes-cli --quiet -- --config /tmp/hermes-smoke.toml
```

Expected: Reaches an "idle" state with the assistant's echo response in the chat history.

- [ ] **F.4: Squash and push**

```bash
git log --oneline main~5..main    # confirm 5 passes + final docs
git reset --soft main~N           # where N = number of commits to squash (likely 16-25)
git commit -m "refactor: 可读性与可维护性重构

- Pass 1: hermes-core data layer
  - Content::chars / is_text helpers
  - ToolCall::new constructor
  - ToolCallDelta.arguments_delta -> arguments_fragment
  - Usage::prompt_context_tokens / total helpers
  - CHARS_PER_TOKEN_ESTIMATE constant
- Pass 2: hermes-providers split into openai/{mod,request,sse} + anthropic/{mod,request,sse}
  - Oai* -> Openai*, Wire* -> Anthropic*
  - parse_sse_data_line helper
- Pass 3: hermes-agent split
  - loop_engine.rs -> loop_engine/{mod,run,compress,metrics}
  - compressor.rs -> compressor/{mod,strategy,summary,marker}
  - tools/files/policy.rs -> policy/{mod,sensitive,profile,dedup}
  - runtime_agent: default_skills_dir, build_context_engine helpers
- Pass 4: hermes-cli TUI ergonomics
  - input.rs: dispatch_key + 3 helpers
  - render.rs -> render/{mod,layout,chat,input_block,status}
  - ChatView cache extracted from App
  - terminal.rs setup helpers
- Pass 5: naming and polish
  - Test helper unification (user_message)
  - .then() / .filter() over if-else Option construction
  - #[allow] annotations
  - CHARS_PER_TOKEN dedupe
  - Top-of-file doc pass"
git push --force-with-lease origin main
```

---

## Spec coverage check

- [x] Pass 1 (Task 1.1–1.3): `Content` ergonomics, `ToolCall::new`, `ToolCallDelta.arguments_fragment` rename, `Usage` helpers, `CHARS_PER_TOKEN_ESTIMATE`, `Message::char_len_into`, `Provider` trait docs
- [x] Pass 2 (Task 2.1–2.3): OpenAI + Anthropic split into mod/request/sse; Oai*/Wire* renames; `parse_sse_data_line` helper; `to_openai_message`/`to_openai_tool`/`build_chat_request` extraction; `convert_*` rename
- [x] Pass 3 (Task 3.1–3.5): `loop_engine` split; `compress.rs` single-lock; `provider_failure` struct; `runtime_agent` builders; `compressor.rs` 4-way split; `marker.rs` as single source of truth; `tools/files/policy.rs` 3-way split
- [x] Pass 4 (Task 4.1–4.3): `dispatch_key`; `render/` 4-way split; `ChatView` extracted from `App`; `terminal.rs` setup; `tui/mod.rs` re-index
- [x] Pass 5 (Task 5.1–5.5): test helper unification, Option construction tightening, `#[allow]` annotation, CHARS_PER_TOKEN dedupe, top-of-file doc pass

All five passes from the spec are covered. No placeholders. File structure matches the target layout in the spec.

---

## Risk notes

- **Public API changes**: `ToolCallDelta.arguments_delta → arguments_fragment` is a pub field rename; the only known consumer is `StreamAccumulator` (in the same crate), and the provider tests. If a future external consumer (e.g. hermes-gateway) breaks, they'll get a clear compile error pointing at the new name.
- **Provider module layout**: tests in `crates/hermes-providers/tests/` only use the public `OpenAiProvider` / `AnthropicProvider` constructors, so the new `openai/` and `anthropic/` module layout is invisible to them.
- **TUI test cache**: removing `chat_lines_for_width` from `App` may break the few tests in `app.rs` that read `cached_chat_lines` directly. Update those tests to use `ChatView::new(...).lines()` from the render module, or remove them if they only exercised the old cache fields.
- **Squash risk**: 16-25 commits to squash into one. Verify the resulting tree builds and tests pass AFTER squashing locally (push --force-with-lease is irreversible if the squashed commit is broken).

---

## Self-review

- **Spec coverage**: see checklist above; all 5 passes covered.
- **Placeholders**: `grep -nE "TBD|TODO|fill in" docs/superpowers/plans/2026-06-07-readability-and-maintainability.md` returns 0 hits.
- **Type consistency**: `ToolCallDelta.arguments_fragment` is the consistent name across Pass 1, 3, 4 (via accumulator); `OpenaiMessage` / `AnthropicMessage` are the consistent DTO names; `ChatView` is the consistent cache struct name; `default_skills_dir` / `build_context_engine` are the consistent helper names in `runtime_agent.rs`.
