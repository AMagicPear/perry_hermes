# Split Large Files — Extract Shared SSE Helper, Split Providers

**Date:** 2026-06-05
**Status:** Designed. Pending user review of this spec before writing the implementation plan.
**Scope:** `hermes-core` (one extraction), `hermes-providers` (one new shared module + two provider-internal splits).

## 1. Goals

The codebase has accumulated 800-line and 540-line files whose **internal duplication** is the real problem, not the line count. After this refactor:

- The two provider implementations share a single SSE frame-buffering helper instead of duplicating the same ~30 lines of byte-stream → SSE-event parsing twice.
- Each provider's `mod.rs` reads as a clean public surface (struct + builders + `impl Provider`), with request serialization and SSE event parsing living in dedicated private modules.
- `StreamAccumulator` lives in its own file, since it is a 130-line cohesive concept (not a part of the `Provider` trait surface).
- No public API path changes — every type that callers import today still lives at the same path.
- All existing tests keep passing; new tests cover the shared helper.

## 2. Non-Goals

Explicitly out of scope (each would be its own brainstorm round):

- **Splitting `hermes-cli/src/main.rs`.** REPL is single-responsibility (`Args` parse, `dispatch`, `run_repl`); the file is short and the parts are tightly coupled at the type level.
- **Splitting `hermes-runtime/src/lib.rs`.** `AIAgent` and `AgentOptions` are small, cohesive, and read together.
- **Splitting `hermes-loop/src/agent.rs`.** It is one state machine in one function; decomposition into private helpers (e.g. `run_one_iteration`, `dispatch_tool_call`) is value-positive but is a separate cleanup pass that needs its own design.
- **Creating a new crate.** All splits stay inside their existing crate.
- **Renaming anything public.** No `use` site outside the affected crates needs to change.
- **Changing provider behavior.** This is a pure refactor — wire format, event dispatch, and emitted deltas are byte-identical.

## 3. Architecture

### 3.1 Before (file sizes)

```
hermes-core/src/provider.rs             468 lines
  └─ Provider trait, Completion, FinishReason, CompletionDelta, ToolCallDelta,
     StreamAccumulator, accumulate_stream, tests

hermes-providers/src/anthropic.rs       796 lines
  └─ AnthropicProvider, request body (wire types + builders + convert_*),
     impl Provider, SSE parsing (AnthropicStreamState + parse_sse_data_payload
     + parse_content_block_start + parse_content_block_delta + usage_delta
     + anthropic_finish_reason), tests

hermes-providers/src/openai.rs          541 lines
  └─ OpenAiProvider, request body (wire types + build_request_body),
     impl Provider, SSE parsing (OpenAiStreamState + parse_sse_data_payload
     + split_think_tag_content), tests
```

### 3.2 After (file layout)

```
hermes-core/src/
  provider.rs        ← Provider trait, Completion, FinishReason, CompletionDelta, ToolCallDelta
                       (small, just the trait + delta types)
  accumulator.rs     ← StreamAccumulator, accumulate_stream, accumulator tests
                       (130 lines of self-contained stream-aggregation logic)

hermes-providers/src/
  lib.rs             ← pub mod sse; pub mod openai; pub mod anthropic; pub mod echo;
  sse.rs             ← sse_frame_stream  ⭐ shared frame-buffering helper
  openai/
    mod.rs           ← OpenAiProvider struct, builders, impl Provider
    request.rs       ← ChatRequest + Oai* wire types + build_request_body
    sse.rs           ← OpenAiStreamState + parse_sse_data_payload + split_think_tag_content
  anthropic/
    mod.rs           ← AnthropicProvider struct, AnthropicRequestOptions, AnthropicThinking,
                       builders, impl Provider
    request.rs       ← MessagesRequest + OutputConfig + Wire* wire types +
                       build_request_body_with_options + convert_tools +
                       convert_messages_to_anthropic + flush_tool_results +
                       content_to_wire_user + content_to_text + build_thinking_param +
                       build_output_config
    sse.rs           ← AnthropicStreamState + parse_sse_data_payload +
                       parse_content_block_start + parse_content_block_delta +
                       usage_delta + anthropic_finish_reason
  echo.rs            ← (unchanged)
```

### 3.3 Component diagram

```
┌────────────────────────────────────────────────────────────────┐
│ hermes-core                                                    │
│                                                                │
│   provider.rs       Provider trait + Completion + FinishReason │
│                       + CompletionDelta + ToolCallDelta        │
│                                                                │
│   accumulator.rs    StreamAccumulator + accumulate_stream      │
│                       (used by Provider::complete default      │
│                        and AgentLoop::run's drive loop)        │
└────────────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────────────┐
│ hermes-providers                                               │
│                                                                │
│   sse.rs            sse_frame_stream (shared frame buffer)     │
│                                                                │
│   openai/           OpenAiProvider                             │
│     mod.rs            struct + impl Provider                  │
│     request.rs        build_request_body                      │
│     sse.rs            OpenAiStreamState + parse_sse_data_     │
│                         payload + split_think_tag_content    │
│                       ──► calls sse::sse_frame_stream         │
│                                                                │
│   anthropic/        AnthropicProvider                         │
│     mod.rs            struct + impl Provider                  │
│     request.rs        build_request_body_with_options + conv  │
│     sse.rs            AnthropicStreamState + parse_sse_data_  │
│                         payload + event-specific parsers      │
│                       ──► calls sse::sse_frame_stream         │
│                                                                │
│   echo.rs           (unchanged)                               │
└────────────────────────────────────────────────────────────────┘
```

`hermes-loop`, `hermes-runtime`, `hermes-cli`, `hermes-tools` are untouched.

### 3.4 Data flow (unchanged — refactor only)

The agent loop's interaction with a provider is byte-for-byte the same:

1. `AgentLoop::run` calls `provider.stream(messages, tools, cancel)`.
2. Provider serializes a request, POSTs, returns a `CompletionStream`.
3. Internally the provider now drives that stream through `sse::sse_frame_stream(bytes, done_sentinel, parse_payload)`, which buffers bytes, splits on `\n\n`, joins `data:` lines, optionally short-circuits on the done sentinel, and calls `parse_payload` for each event.
4. The closure passed to `sse_frame_stream` is the provider's per-event parser (which holds the provider's stream-state struct).
5. Deltas flow back to the loop, the loop accumulates via `StreamAccumulator`, breaks on `finish_reason`.

## 4. Core types

### 4.1 `sse_frame_stream` (new in `hermes-providers/src/sse.rs`)

```rust
/// Parse an HTTP byte stream (typically `reqwest::Response::bytes_stream()`)
/// as Server-Sent Events and yield `CompletionDelta`s as each event arrives.
///
/// The helper handles SSE framing only: byte buffer accumulation, splitting
/// on `\n\n`, joining multi-line `data:` payloads (per the SSE spec), and
/// skipping non-data lines (comments, event-type lines, etc.). It does NOT
/// know anything about any specific provider's event schema — that is the
/// `parse_payload` closure's job.
///
/// `done_sentinel`: if `Some(s)`, a payload that equals `s` after joining
/// `data:` lines terminates the stream without yielding. Anthropic passes
/// `None`; OpenAI passes `Some("[DONE]")`.
///
/// `parse_payload`: invoked once per complete SSE event with the joined
/// `data:` payload. Return:
///   - `Ok(Some(delta))` — yield this delta to the consumer
///   - `Ok(None)` — silently skip this event (e.g. ping, content_block_stop)
///   - `Err(provider_error)` — propagate and terminate the stream
///
/// The closure is `FnMut + Send + 'static` so it can own provider-specific
/// stream state (e.g. `AnthropicStreamState`).
pub fn sse_frame_stream<S, F>(
    bytes: S,
    done_sentinel: Option<&'static str>,
    parse_payload: F,
) -> impl Stream<Item = Result<CompletionDelta, ProviderError>> + Send
where
    S: Stream<Item = reqwest::Result<Bytes>> + Unpin + Send + 'static,
    F: FnMut(&str) -> Result<Option<CompletionDelta>, ProviderError> + Send + 'static,
{
    async_stream::stream! {
        let mut buffer = String::new();
        let mut bytes = Box::pin(bytes);
        while let Some(chunk) = bytes.next().await {
            match chunk {
                Ok(c) => buffer.push_str(&String::from_utf8_lossy(&c)),
                Err(e) => { yield Err(ProviderError::Transport(e)); return; }
            }
            while let Some(pos) = buffer.find("\n\n") {
                let event: String = buffer.drain(..pos + 2).collect();
                let payload: String = event
                    .lines()
                    .filter_map(|line| line.strip_prefix("data: "))
                    .collect::<Vec<_>>()
                    .join("\n");
                if let Some(s) = done_sentinel {
                    if payload == s { return; }
                }
                if payload.is_empty() { continue; }
                match parse_payload(&payload) {
                    Ok(Some(delta)) => yield Ok(delta),
                    Ok(None) => {}
                    Err(e) => { yield Err(e); return; }
                }
            }
        }
    }
}
```

### 4.2 Why join `data:` lines (instead of taking only the first one)?

- **Anthropic's existing behavior** is to join — its `parse_sse_chunks` does
  `event.lines().filter_map(|line| line.strip_prefix("data: ")).collect::<Vec<_>>().join("\n")`.
- **OpenAI's existing behavior** is to iterate and yield one delta per `data:` line, but
  in practice OpenAI streams always have exactly one `data:` line per event, so joining
  produces an equivalent result.
- **SSE spec** (WHATWG) explicitly allows multiple `data:` lines in a single event that
  must be joined. Joining is the more spec-compliant default and removes a subtle
  compatibility risk for future providers.

### 4.3 Provider call sites

**OpenAI** (in `openai/mod.rs::impl Provider::stream`):

```rust
let mut state = OpenAiStreamState::default();
let stream = sse::sse_frame_stream(
    response.bytes_stream(),
    Some("[DONE]"),
    move |payload| openai::sse::parse_sse_data_payload(payload, &mut state)
        .map(Some),
);
Ok(Box::pin(stream))
```

**Anthropic** (in `anthropic/mod.rs::impl Provider::stream`):

```rust
let mut state = AnthropicStreamState::default();
let stream = sse::sse_frame_stream(
    response.bytes_stream(),
    None,
    move |payload| anthropic::sse::parse_sse_data_payload(payload, &mut state),
);
Ok(Box::pin(stream))
```

Note: `AnthropicStreamState` and `OpenAiStreamState` are owned by the closure via `move`,
so the returned stream is `'static` (it doesn't borrow from any local).

### 4.4 `StreamAccumulator` (moved to `hermes-core/src/accumulator.rs`)

The struct, its `Default` impl, `add` / `finalize` / `is_empty` / `into_partial_message`
methods, the `accumulate_stream` free function, and the existing test module all move
verbatim from `provider.rs` to a new `accumulator.rs`. The `Provider` trait's default
`complete()` impl now reads `crate::accumulator::accumulate_stream`.

**Public path preservation**: `crates/hermes-loop/src/agent.rs:186` imports
`hermes_core::provider::StreamAccumulator`. To keep this import working without touching
`agent.rs`, `provider.rs` re-exports the moved items at the bottom of the file:

```rust
// hermes-core/src/provider.rs
pub use crate::accumulator::{accumulate_stream, StreamAccumulator};
```

`hermes-core/src/lib.rs` is unchanged (it does not currently re-export `StreamAccumulator`
from the crate root, and that remains the case).

`hermes-core/src/provider.rs` shrinks to ~280 lines (just the trait + delta types + their tests).

## 5. Implementation strategy (TDD per CLAUDE.md)

Per `CLAUDE.md` "TDD Workflow" — strict RED → GREEN → REFACTOR.

### Step 1: RED — write failing tests for the new helper

In `hermes-providers/src/sse.rs`, write a `#[cfg(test)] mod tests` block with three tests:

1. `sse_frame_stream_yields_deltas_for_data_events` — feed bytes containing two `data:` events separated by `\n\n`, assert two `Ok(CompletionDelta { content_delta: Some(...), .. })` items arrive.
2. `sse_frame_stream_returns_on_done_sentinel` — feed bytes ending with `data: [DONE]\n\n` and a `done_sentinel = Some("[DONE]")`, assert the stream yields nothing after the sentinel and ends.
3. `sse_frame_stream_skips_empty_payloads` — feed bytes with an event whose only data is empty (a `ping` would do), assert no delta is yielded.

`cargo test -p hermes-providers` — confirm these fail (helper doesn't exist yet).

### Step 2: GREEN — implement `sse_frame_stream`

Copy the function from §4.1 above. Run tests until they pass.

### Step 3: REFACTOR — wire up the providers

In order of risk (smallest delta first):

1. **OpenAI**: in `openai.rs`, replace the `parse_sse_chunks` definition with a thin
   wrapper that delegates to `sse::sse_frame_stream` plus a closure wrapping
   `parse_sse_data_payload`. Run `cargo test -p hermes-providers`. All existing
   `tests/openai.rs` integration tests must still pass — the public `OpenAiProvider::stream`
   behavior is unchanged.

2. **Anthropic**: same pattern, no `.map(Some)` wrapper (Anthropic's
   `parse_sse_data_payload` already returns `Result<Option<...>, _>`). Run
   `cargo test -p hermes-providers`. `tests/anthropic.rs` must still pass.

3. **Extract `StreamAccumulator`**: move the struct + tests from `provider.rs` to a
   new `accumulator.rs`. Re-export from `provider.rs` (or `lib.rs`). Run
   `cargo test -p hermes-core` and the dependent crate tests.

4. **Split `openai.rs` into `openai/` directory**:
   - `openai/mod.rs` keeps `OpenAiProvider` struct + builder methods + `impl Provider`.
   - `openai/request.rs` takes the `ChatRequest` / `Oai*` wire types + `build_request_body`.
   - `openai/sse.rs` takes `OpenAiStreamState` + `parse_sse_data_payload` + `split_think_tag_content`.
   - `crates/hermes-providers/src/lib.rs`: change `pub mod openai;` (still works — Rust
     resolves `openai.rs` OR `openai/mod.rs` OR `openai/` directory).
   - **Test migration**: all 9 unit tests in `openai.rs::mod tests` are SSE-parser
     tests (they all use the `parse_sse_for_test` helper which wraps `parse_sse_chunks`).
     The whole `mod tests` block moves to `openai/sse.rs`. The test helper
     `parse_sse_for_test` is rewritten to call `sse::sse_frame_stream` directly with a
     fresh `OpenAiStreamState` (the existing one-liner becomes ~10 lines because the
     closure now owns state — this is fine; the test body is unchanged).

5. **Split `anthropic.rs` into `anthropic/` directory**:
   - `anthropic/mod.rs`: `AnthropicProvider` struct, `AnthropicRequestOptions`,
     `AnthropicThinking`, builders, `impl Provider`.
   - `anthropic/request.rs`: `MessagesRequest` / `OutputConfig` / `Wire*` / `ThinkingParam`
     + `build_request_body_with_options` + all `convert_*` / `content_to_*` / `build_*`
     helpers.
   - `anthropic/sse.rs`: `AnthropicStreamState` + `parse_sse_data_payload` +
     `parse_content_block_start` + `parse_content_block_delta` + `usage_delta` +
     `anthropic_finish_reason`.
   - **Test migration**: `anthropic.rs::mod tests` has 9 tests — 7 of them
     (`convert_messages_pulls_system_out_and_joins_text_parts`,
     `convert_messages_emits_tool_use_and_tool_result_blocks`,
     `request_body_uses_structured_tool_choice_and_input_schema`,
     `thinking_defaults_to_off_for_claude_3_7`,
     `manual_thinking_is_explicit`,
     `adaptive_thinking_is_explicit`,
     `adaptive_thinking_can_set_effort`) test request-body construction → move to
     `anthropic/request.rs::mod tests`. The other 2 (`parses_text_tool_and_usage_stream`,
     `message_delta_usage_can_update_input_tokens`) test the SSE parser → move to
     `anthropic/sse.rs::mod tests`. Each module's test block keeps the same
     `use super::*;` plus any additional `use` it needs.

### Step 4: Verify

```bash
cargo build
cargo test                                          # all unit + integration tests
cargo test -p hermes-providers                      # focus on the changed crate
cargo test -p hermes-core                           # StreamAccumulator moved
cargo clippy --all-targets --all-features -- -D warnings
```

Every test that passed before this refactor must pass after. The new `sse::tests` block
adds three more green tests.

### Step 5: Manual smoke (optional)

If `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` are set in `.envrc`:

```bash
echo "hello" | cargo run -p hermes-cli --quiet -- --provider openai
echo "hello" | cargo run -p hermes-cli --quiet -- --provider anthropic
```

Both should produce a response and exit cleanly — confirming that the new SSE helper
is exercised end-to-end.

## 6. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Forgetting to mark `FnMut` as `move` causes the returned stream to borrow from a local | Use `move` on both provider closures; lint catches non-`'static` future returns if a `Box::pin` call site is changed. |
| SSE spec compliance: joining `data:` lines changes behavior for a provider that genuinely sends multi-line data and a parser that expects only the first line | OpenAI never does this; Anthropic already joins. If a future provider is added and hits this, the helper can grow a `join_data_lines: bool` flag — but YAGNI for now. |
| Public API accidentally re-exported at the wrong path | `hermes-providers/src/lib.rs` re-exports `pub use openai::OpenAiProvider;` etc. explicitly so downstream code (`hermes-runtime`) keeps working. Run `cargo build` of `hermes-runtime` and `hermes-cli` after the split to catch any miss. |
| Test file moves break test discovery | Each moved `#[cfg(test)] mod tests` keeps the same `use super::*;` shape; running `cargo test -p hermes-providers` confirms they still execute. |
| `parse_sse_data_payload` signatures diverge between OpenAI and Anthropic | They already diverge today (`Result<Option<...>>` vs `Result<...>`). The new shared helper uses `FnMut(&str) -> Result<Option<CompletionDelta>, _>` and OpenAI adapts with `.map(Some)`. Documented in §4.3. |

## 7. Test coverage summary

**New tests** (3, in `hermes-providers/src/sse.rs::tests`):
- Yields deltas for two `data:` events.
- Terminates on `[DONE]` sentinel.
- Skips empty payloads (pings).

**Existing tests, unchanged behavior**:
- `crates/hermes-core/src/accumulator.rs::tests` — all move with `StreamAccumulator`.
- `crates/hermes-providers/tests/openai.rs` — `OpenAiProvider::stream` integration tests (HTTP-mock).
- `crates/hermes-providers/tests/anthropic.rs` — same.
- `crates/hermes-providers/tests/openai_stream.rs` — SSE byte-stream test.
- `crates/hermes-loop/tests/echo_loop.rs`, `tool_dispatch.rs`, `arg_validation.rs`, `usage_metrics.rs` — exercise the loop with the providers; behavior unchanged.
- `crates/hermes-cli/src/main.rs` smoke (offline via `echo`) and `live_tool_use` example — unchanged.

## 8. Out-of-scope follow-ups (parking lot)

These were considered and deferred. Each is a separate brainstorm.

- **Decompose `AgentLoop::run` into private helpers** (`run_one_iteration`, `dispatch_tool_call`, `append_tool_result`). Improves readability of the 200-line method.
- **Split `hermes-runtime/src/lib.rs`** if it grows past ~300 lines.
- **Split `hermes-cli/src/main.rs`** only if `run_repl` grows past ~250 lines.
- **`sse::sse_frame_stream` → `sse::SseParser` struct** if we ever need named config knobs (join behavior, custom split delimiter). YAGNI today.
