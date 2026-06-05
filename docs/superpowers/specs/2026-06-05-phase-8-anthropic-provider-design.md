# Phase 8 — Anthropic Provider

**Date:** 2026-06-05
**Status:** Implemented in this branch.
**Scope implemented:** `hermes-providers` (new `AnthropicProvider`), `hermes-runtime` (new `AIAgent::anthropic` constructors), `hermes-cli` (new `--provider anthropic` flag).

## 1. Goals

Add a second wire-protocol implementation of the `Provider` trait that talks to Anthropic's Messages API. After this phase:

- A user can run `hermes --provider anthropic` against Anthropic and Anthropic-compatible endpoints and get the same streaming + tool-calling experience the OpenAI path delivers.
- The `Message` / `Role` / `Content` / `ContentPart` model in `hermes-core` continues to work unchanged — the Anthropic adapter translates at the wire boundary.
- The agent loop, runtime facade, and CLI all work with `AnthropicProvider` without modification.
- The shape of `AnthropicProvider` (struct + `new()` + `with_base_url()` + optional `with_api_key_header()`) mirrors `OpenAiProvider` closely enough that a future config-file layer (`ProviderConfig` enum + `AIAgent::from_config`) can dispatch on kind without touching provider internals.

## 2. Non-Goals

Explicitly deferred to later phases (these would each be their own brainstorm round):

- **OAuth / setup-tokens / Claude Code credentials / Entra ID bearer auth.** Phase 8 sends the API key as a configurable static header. The default is Anthropic's `x-api-key`; `ANTHROPIC_API_KEY_HEADER=api-key` supports MiMo-style compatible endpoints. The `_is_oauth_token` / `_requires_bearer_auth` / `read_claude_code_credentials` / `refresh_anthropic_oauth_pure` machinery in Hermes's adapter is not ported.
- **Prompt caching** (`cache_control` blocks on system / messages / tools). Not in the request body. Hermes's `prompt_caching.py` and `_evict_old_screenshots` are not ported.
- **Third-party endpoint detection** (MiniMax Bearer auth, Azure `api-version` query param, Kimi `/coding` User-Agent spoof, DeepSeek `/anthropic` round-trip rules, AWS Bedrock model-id shape). `with_base_url()` and `with_api_key_header()` cover simple compatible hosts, but no endpoint-specific behaviors are baked in.
- **Multimodal input** (`ContentPart::ImageUrl` → Anthropic `image` source block with base64 / media_type). `Content::Parts` is serialized as plain text by joining text parts with `\n`; image parts are dropped in Phase 8.
- **Per-model `max_tokens` resolution table** (Hermes has a 20+ entry dict for Claude 3/3.5/3.7/4/4.5/4.6/4.7/4.8 plus third-party overrides). Phase 8 hard-codes a single `max_tokens = 16384` in the request body. This is a pragmatic default for the provider abstraction; a future phase can introduce a model→tokens table and lower the value for models/endpoints that reject it.
- **Per-model sampling-param restrictions** (Opus 4.7+ rejects any non-default `temperature` / `top_p` / `top_k`). The current implementation no longer infers thinking from model names. Anthropic thinking is off by default and only sent when a config-driven `AnthropicRequestOptions` explicitly requests manual or adaptive thinking.
- **Fast mode** (Opus 4.6 only, `extra_body.speed = "fast"` + `fast-mode-2026-02-01` beta header). Not sent.
- **Bedrock / Vertex / Azure-native** clients. Out of scope; `AnthropicProvider` is direct-Messages-API only.
- **The `ProviderConfig` enum / `AIAgent::from_config` factory / `--config` CLI flag.** This is a follow-up phase. Phase 8's `AIAgent::anthropic()` is the building block that future config code calls.

## 3. Architecture

### 3.1 Component diagram

```
┌──────────────────────┐
│ hermes-cli (REPL)    │  --provider anthropic  (new flag)
│ main.rs              │
└──────────┬───────────┘
           │ uses
           ▼
┌──────────────────────┐
│ hermes-runtime       │  AIAgent::anthropic(...)
│ lib.rs               │  AIAgent::anthropic_with_api_key_header(...)
└──────────┬───────────┘
           │ holds
           ▼
┌──────────────────────┐
│ hermes-loop          │  AgentLoop::run unchanged from Phase 5/6
│ agent.rs             │  (no provider-specific knowledge)
└──────────┬───────────┘
           │ calls
           ▼
┌──────────────────────┐
│ hermes-providers     │  AnthropicProvider::stream (new)
│ anthropic.rs         │    — POST {base}/messages with configurable API-key header + anthropic-version
│                      │    — wire serialization (system→top-level, tool→user+tool_result)
│                      │    — explicit config-driven thinking only; no historical thinking replay
│                      │    — Anthropic SSE → CompletionDelta (event-type dispatch)
│                      │  OpenAiProvider::stream (unchanged)
│                      │  EchoProvider::stream (unchanged)
└──────────────────────┘
```

No `hermes-core` changes. The `Provider` trait and `CompletionDelta` / `StreamAccumulator` abstractions are reused as-is.

### 3.2 Data flow (one turn, no tools)

1. CLI / runtime builds an `AnthropicProvider` from `(api_key, model, base_url, api_key_header)` and hands it to `AgentLoop`.
2. Loop calls `provider.stream(&messages, &tools, cancel)`.
3. Anthropic provider serializes:
   - Extract `system` from messages into a top-level field.
   - Convert remaining messages: assistant text → `content: [{type: "text", text}]`; assistant tool_calls → append `tool_use` blocks with parsed `arguments`; historical assistant reasoning is not serialized back as `thinking` because `Message` does not preserve Anthropic signatures; user text → `content: text`; tool role → merge into trailing user message as `tool_result` block.
   - Convert tools: drop the OpenAI `function` wrapper.
   - Add `thinking` param only when explicitly configured.
4. POST to `{base_url}/messages` with API-key header (`x-api-key` by default), `anthropic-version: 2023-06-01`, `content-type: application/json`, body `{model, system?, messages, tools?, max_tokens: 16384, stream: true, thinking?, temperature?}`.
5. On 401 → `ProviderError::Auth`. On 429 → `ProviderError::RateLimited`. On other non-2xx → `ProviderError::InvalidResponse`.
6. SSE parser consumes the byte stream and yields `CompletionDelta`s. The loop drives the stream (existing `tokio::select!` with cancel), emits `ContentDelta` / `ReasoningDelta` / `ToolCallPartial` events, accumulates into `StreamAccumulator`, breaks on `finish_reason`.
7. `acc.finalize()` returns a `Completion` with `message.reasoning` carrying any streamed `thinking` text, `message.tool_calls` carrying assembled `tool_use` calls, `usage` carrying `input_tokens` + `output_tokens` (summed across two SSE events), and `finish_reason` translated from `stop_reason`.

### 3.3 Data flow (one turn, with tool calls)

Same as OpenAI (Phase 5 §3.3). Anthropic's stream:

```
event: content_block_start
data: {... "content_block": { "type": "tool_use", "id": "toolu_...", "name": "bash" }}

event: content_block_delta
data: {... "delta": { "type": "input_json_delta", "partial_json": "{\"command\":" }}

event: content_block_delta
data: {... "delta": { "type": "input_json_delta", "partial_json": "\"ls\"}" }}

event: message_delta
data: {... "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 42 }}
```

Each event is dispatched (see §5.5):
- `content_block_start` with `type=tool_use` → `CompletionDelta` with `tool_call_delta = Some(ToolCallDelta { index, id: Some(...), name: Some(...), arguments_delta: None })`.
- `content_block_delta` with `type=input_json_delta` → `tool_call_delta.arguments_delta = Some(partial_json)`.
- `message_delta` → `finish_reason = ToolUse`, plus `usage.output_tokens` summed into accumulated `Usage`.

`StreamAccumulator` aggregates `ToolCallDelta`s by `index` (existing logic, untouched). The loop dispatches the assembled tool call (existing logic, untouched) and re-enters the loop with the tool's output appended as a `role: Tool` message.

## 4. Core types

**No `hermes-core` changes.** All new code lives in `crates/hermes-providers/src/anthropic.rs` and a few small additions to `crates/hermes-runtime/src/lib.rs` and `crates/hermes-cli/src/main.rs`.

## 5. Provider implementation

### 5.1 Struct + constructors (in `hermes-providers/src/anthropic.rs`)

```rust
pub struct AnthropicProvider {
    api_key: String,
    api_key_header: String,
    model: String,
    base_url: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            api_key_header: "x-api-key".into(),
            base_url: "https://api.anthropic.com/v1".into(),
            model: model.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn with_api_key_header(mut self, header_name: impl Into<String>) -> Self {
        self.api_key_header = header_name.into();
        self
    }
}
```

Mirrors `OpenAiProvider` closely. No `with_model` (the user picks at construction; runtime can build a new provider per model if needed). `with_api_key_header()` is intentionally narrow: it supports simple Anthropic-compatible endpoints such as MiMo without adding domain-specific endpoint detection.

### 5.2 Request body

```rust
#[derive(Serialize)]
struct MessagesRequest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<WireToolChoice>,    // { "type": "auto" } — see §5.4
    max_tokens: u32,                        // hard-coded 16384
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingParam>,        // explicit only; default omitted
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,               // only 1.0, only for explicit manual thinking
}

#[derive(Serialize)]
struct WireMessage {
    role: String,                           // "user" | "assistant"
    content: WireMessageContent,            // string | Vec<WireContentBlock>
}

#[derive(Serialize)]
#[serde(untagged)]
enum WireMessageContent {
    Text(String),
    Blocks(Vec<WireContentBlock>),
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String, #[serde(skip_serializing_if = "Option::is_none")] is_error: Option<bool> },
    // Image part intentionally omitted in Phase 8 (see §2).
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireToolChoice {
    Auto,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ThinkingParam {
    Enabled { budget_tokens: u32 },
    Adaptive { display: String },
}
```

### 5.3 Message conversion (`convert_messages_to_anthropic`)

Returns `(Option<String> /* system */, Vec<WireMessage>)`.

1. Iterate `messages`:
   - `Role::System` → set `system = Some(content.text())`. The `AgentLoop` already guarantees at most one system message; if more arrive, the last one wins (defensive — not exercised in tests).
   - `Role::User`:
     - If `content` is `Content::Text(s)` → push `WireMessage { role: "user", content: WireMessageContent::Text(s) }`.
     - If `content` is `Content::Parts(parts)`:
       - If all parts are `ContentPart::Text` → join them with `\n` into a single text message (Anthropic accepts strings directly).
      - If any part is `ContentPart::ImageUrl` → drop the image and emit a text-only message. (Phase 8 limitation; see §2.)
   - `Role::Assistant`:
     - `content` → emit text block.
     - `reasoning` is intentionally ignored on request serialization in Phase 8. Anthropic thinking blocks include signatures that must be preserved across turns; `Message` currently stores text reasoning only.
     - `tool_calls` → for each call, parse `arguments` (`serde_json::Value`) and emit a `ToolUse` block with `input = arguments`. We don't re-stringify — Anthropic accepts objects.
   - `Role::Tool`:
     - Push the tool result into a pending `tool_results: Vec<ToolResult>` accumulator. Don't emit a message yet.
2. **Merge pending tool results** into the previous emitted message if it is `role: user`; otherwise start a new user message with `content = Blocks([ToolResult, ...])`. This matches Hermes's `_convert_tool_message_to_result` and its merge-consecutive behavior.
3. No orphan-block defensive strip in Phase 8 (see §2 — out of scope; trust the loop to maintain order).

### 5.4 Tool choice

Phase 8 sends structured `tool_choice: { "type": "auto" }` when `tools` is non-empty. When `tools` is empty, omit the field.

Hermes maps `tool_choice == "required"` to `{type: "any"}` and `tool_choice == "none"` to omitting tools entirely. Phase 8 does not model `tool_choice` in `AgentLoop` / `LoopConfig` (no caller exercises it), so we only emit `"auto"`. A future phase that surfaces `tool_choice` will need a richer mapping.

### 5.5 SSE parser

```rust
fn parse_sse_data_payload(payload: &str) -> Result<Option<CompletionDelta>, ProviderError>;
```

Dispatches on the JSON `type` field:

| Event `type` | Action | Notes |
|---|---|---|
| `message_start` | Yield `CompletionDelta { usage: Some(Usage { input_tokens, output_tokens: 0, .. }), ..None }` | Input token count arrives here. |
| `content_block_start` (`text`) | Yield `None` (silent) | The first text block opens with empty text; nothing to stream yet. |
| `content_block_start` (`tool_use`) | Yield `Some(ToolCallDelta { index, id, name, arguments_delta: None })` | `index` from the event. |
| `content_block_start` (`thinking`) | Yield `None` | We don't surface "thinking started" — only its content as it streams. |
| `content_block_delta` (`text_delta`) | Yield `content_delta = Some(text)` | |
| `content_block_delta` (`input_json_delta`) | Yield `tool_call_delta.arguments_delta = Some(partial_json)` | |
| `content_block_delta` (`thinking_delta`) | Yield `reasoning_delta = Some(thinking)` | |
| `content_block_delta` (`signature_delta`) | Drop silently | We don't preserve signatures. |
| `content_block_stop` | Yield `None` | Block boundary; no delta needed. |
| `message_delta` | Update internal `usage.output_tokens` accumulator; yield `finish_reason = mapped_stop_reason` and `usage = Some(current_accumulated_usage)` | Two-stage usage: input from `message_start`, output from `message_delta`. |
| `message_stop` | Yield `None` | Stream terminator; outer loop breaks. |
| `ping` | Yield `None` | Keep-alive. |
| `error` | Yield `Err(ProviderError::InvalidResponse(error.message))` | |
| anything else | Yield `Err(ProviderError::InvalidResponse(format!("unknown event: {type}")))` | Defensive — fail loud. |

`parse_sse_chunks` is the byte-buffer-and-event-boundary driver, parallel to the OpenAI one. It walks `\n\n` boundaries, extracts `event:` + `data:` lines per Anthropic's spec, ignores `:` comment lines and unknown event types per spec, and feeds `data:` payloads to `parse_sse_data_payload`. Cancellation: the outer `tokio::select!` in the loop drops the stream; the parser simply stops being polled.

### 5.6 Thinking

```rust
#[derive(Debug, Clone, Default)]
pub struct AnthropicRequestOptions {
    pub thinking: Option<AnthropicThinking>,
}

#[derive(Debug, Clone)]
pub enum AnthropicThinking {
    Manual { budget_tokens: u32 },
    Adaptive {
        display: String,
        effort: Option<String>,
    },
}
```

Thinking is config-driven. The default request sends no `thinking` field, which is the safest behavior for Anthropic-compatible third-party APIs. Official Anthropic users can opt into `manual` or `adaptive` thinking through Phase 9 config; manual thinking sets `temperature = 1.0`, adaptive thinking can optionally set `output_config.effort` and does not send temperature.

### 5.7 Finish reason

```rust
fn anthropic_finish_reason(s: &str) -> FinishReason {
    match s {
        "end_turn" => FinishReason::Stop,
        "tool_use" => FinishReason::ToolUse,
        "max_tokens" => FinishReason::Length,
        "stop_sequence" => FinishReason::Stop,
        "refusal" => FinishReason::ContentFilter,
        _ => FinishReason::Error,
    }
}
```

This is a private function in `anthropic.rs`; we do not extend `FinishReason::from_provider_str` (which is OpenAI's name in the public API). A future phase that adds Gemini / Bedrock can either generalize `from_provider_str` or keep per-provider mappers; the choice is deferred.

## 6. Runtime + CLI wiring

### 6.1 `AIAgent::anthropic` (in `hermes-runtime/src/lib.rs`)

```rust
impl AIAgent {
    pub fn anthropic(
        api_key: impl Into<String>,
        model: impl Into<String>,
        base_url: impl Into<String>,
        options: AgentOptions,
    ) -> Self {
        Self::new(
            AnthropicProvider::new(api_key, model).with_base_url(base_url),
            options,
        )
    }
}
```

Mirrors `openai_compatible` exactly. No additional imports beyond the existing `use hermes_providers::...` line.

### 6.2 CLI `--provider anthropic` (in `hermes-cli/src/main.rs`)

Add `"anthropic"` to the `match args.provider.as_str()` arms, alongside `"openai"` and `"echo"`. The arm reads these env vars / args:

- `ANTHROPIC_API_KEY`
- `ANTHROPIC_MODEL` (default `claude-sonnet-4-5`)
- `ANTHROPIC_BASE_URL` (default `https://api.anthropic.com/v1`)
- `ANTHROPIC_API_KEY_HEADER` (default `x-api-key`; use `api-key` for MiMo)

The existing CLI structure (multi-turn REPL, streaming rendering, Ctrl-C) works without modification — it already goes through `AIAgent`.

MiMo-compatible manual test:

```bash
export ANTHROPIC_API_KEY="<mimo key>"
export ANTHROPIC_MODEL=mimo-v2.5
export ANTHROPIC_BASE_URL=https://api.xiaomimimo.com/anthropic/v1
export ANTHROPIC_API_KEY_HEADER=api-key
cargo run -p hermes-cli -- --provider anthropic
```

## 7. Error handling

Inherits the existing provider error semantics. No new error variants are added to `hermes_core::ProviderError`. The mapping is:

| Anthropic surface | `ProviderError` variant | Notes |
|---|---|---|
| HTTP 401 | `Auth(body)` | Body is the JSON error message. |
| HTTP 429 | `RateLimited { retry_after_secs: 1 }` | Same as OpenAI. A future phase can read the `retry-after` header. |
| HTTP 400 (bad request) | `InvalidResponse(body)` | Includes Anthropic's "thinking is not enabled on this model" type errors. |
| HTTP 5xx | `InvalidResponse(body)` | |
| Transport failure (reqwest) | `Transport(reqwest::Error)` | |
| SSE `error` event | `InvalidResponse(event.message)` | |
| SSE parse failure | `InvalidResponse(format!("sse json: {e}"))` | |
| Unknown event type | `InvalidResponse(format!("unknown event: {type}"))` | Fail loud. |
| Cancellation (from outside) | `Cancelled` | Outer loop handles. |

Mid-stream errors propagate through the byte stream as `Some(Err(ProviderError))`. The loop's existing behavior (Phase 5/6 §6.3) discards already-emitted deltas on stream error.

## 8. Testing

### 8.1 Unit tests (in `crates/hermes-providers/src/anthropic.rs`)

| Test | Covers |
|---|---|
| `convert_messages_pulls_system_out_of_messages` | System → top-level field |
| `convert_messages_preserves_user_text` | Plain user text → `WireMessageContent::Text` |
| `convert_messages_joins_text_parts` | `Content::Parts([Text, Text])` → joined string |
| `convert_messages_drops_image_part` | Image → text fallback (Phase 8 limitation) |
| `convert_messages_emits_tool_use_blocks_for_assistant` | Tool calls → `ToolUse` blocks, `arguments` as object |
| `convert_messages_does_not_replay_unsigned_reasoning` | Reasoning text is not serialized as `thinking` |
| `convert_messages_merges_consecutive_tool_results_into_user` | Multiple `Role::Tool` → one user message with `tool_result` blocks |
| `convert_messages_renames_tool_call_id_to_tool_use_id` | Field mapping |
| `convert_messages_handles_assistant_with_text_and_tools` | Mixed text + tool_use on same assistant turn |
| `convert_tools_strips_function_wrapper` | Tool schema conversion |
| `thinking_defaults_to_off_for_claude_3_7` | No model-name inference |
| `manual_thinking_is_explicit` | `thinking.type = "enabled"` with `budget_tokens: 8000` and `temperature: 1` |
| `adaptive_thinking_is_explicit` | `thinking.type = "adaptive"` with display mode and no temperature |
| `adaptive_thinking_can_set_effort` | `output_config.effort` is included only when configured |
| `parse_message_start_yields_input_usage` | SSE → `usage` |
| `parse_content_block_delta_text_yields_content_delta` | text_delta |
| `parse_content_block_delta_input_json_yields_arguments_delta` | input_json_delta |
| `parse_content_block_delta_thinking_yields_reasoning_delta` | thinking_delta |
| `parse_content_block_delta_signature_is_silently_dropped` | signature_delta → no delta |
| `parse_content_block_start_tool_use_yields_id_and_name` | tool_use start |
| `parse_message_delta_yields_finish_reason` | end_turn, tool_use, max_tokens, refusal mappings |
| `parse_message_delta_accumulates_output_tokens` | Output usage updates |
| `parse_message_stop_yields_no_delta` | Stream terminator |
| `parse_error_event_yields_invalid_response` | Error event |
| `parse_unknown_event_type_yields_error` | Defensive |
| `parse_sse_chunks_handles_full_message_with_text_and_tools` | End-to-end streaming parse |
| `parse_sse_chunks_handles_done_with_message_stop` | Stream ends cleanly |
| `anthropic_finish_reason_maps_end_turn_and_tool_use` | FinishReason mapping |

### 8.2 Integration tests (in `crates/hermes-providers/tests/anthropic.rs`)

| Test | Covers |
|---|---|
| `anthropic_provider_maps_401_to_auth_error` | Pre-stream error path |
| `anthropic_provider_maps_429_to_rate_limited` | Pre-stream error path |
| `anthropic_provider_posts_to_messages_endpoint_with_version_header` | URL + `x-api-key` + `anthropic-version` |
| `anthropic_provider_can_use_custom_api_key_header` | MiMo-style `api-key` header |
| `anthropic_provider_sends_top_level_system_field` | System is NOT inside `messages` array |
| `anthropic_provider_sends_tool_use_blocks_not_function_wrapper` | Tool serialization |
| `anthropic_provider_streams_text_and_tool_use_end_to_end` | Whole flow: connect, parse SSE, accumulate, return `Completion` |
| `anthropic_provider_sends_structured_tool_choice` | Request body has `tool_choice = { "type": "auto" }` |
| `anthropic_provider_omits_thinking_by_default` | Request body has no `thinking` field unless configured |
| `anthropic_provider_runs_two_turn_loop_with_tool_call` | End-to-end: `AgentLoop` with a `ScriptedAnthropicServer` returning tool_use → tool result → end_turn |

Tests 1–9 use `httpmock`. Test 10 uses `httpmock` for the request body assertions plus the existing `AgentLoop` from `hermes-loop` to drive a real two-turn flow (mirroring `openai_stream.rs`'s end-to-end test).

### 8.3 No live smoke

No `examples/anthropic_smoke.rs` that talks to api.anthropic.com. A future phase can add one (the `--provider anthropic` CLI path will exercise the same code), gated on `ANTHROPIC_API_KEY` env var presence. Skipping this for Phase 8 keeps CI deterministic and matches the spirit of "validating the abstraction" rather than "shipping a production Anthropic client."

## 9. Migration & rollout

- New code only. No modifications to `OpenAiProvider`, `EchoProvider`, `AgentLoop`, `StreamAccumulator`, `Provider` trait, or any `hermes-core` type.
- `lib.rs` of `hermes-providers` gains `pub mod anthropic;` and `pub use anthropic::AnthropicProvider;`.
- `hermes-runtime/src/lib.rs` adds Anthropic constructors and one `use` line.
- `hermes-cli/src/main.rs` adds one match arm.
- No public API breakage.

## 10. Open follow-ups (out of scope for this round)

- **`ProviderConfig` enum + `AIAgent::from_config` + `--config` CLI flag.** Builds on `AIAgent::anthropic` and `AIAgent::openai_compatible` as the dispatch targets.
- **OAuth / setup-token / Claude Code credentials / Entra ID bearer auth** (port of Hermes's `anthropic_adapter.py` auth detection).
- **Prompt caching** (`cache_control` on system, messages, tools).
- **Per-model `max_tokens` resolution table** (the 20+ entry dict in Hermes's `_ANTHROPIC_OUTPUT_LIMITS`).
- **Multimodal input** (Anthropic `image` source block with base64 + media_type from `ContentPart::ImageUrl`).
- **Third-party endpoint detection** (MiniMax Bearer, Azure `api-version`, Kimi `/coding` UA spoof, DeepSeek `/anthropic` round-trip rules, AWS Bedrock model-id shape). Simple custom API-key header support exists; richer auth/routing remains out of scope.
- **Fast mode** (Opus 4.6 only).
- **Per-model sampling param restrictions** (4.7+ stripping).
- **Orphan tool block defensive strip** in `convert_messages_to_anthropic` (a `ContextCompressor` concern; Phase 7 territory).
- **Preserved thinking blocks with signatures** (would require extending `Message` with a side channel for raw `thinking` blocks; Phase 8 does not replay historical reasoning as Anthropic `thinking` blocks).
- **Reading `retry-after` from 429 responses** (Hermes hardcodes 1s; we do the same).
