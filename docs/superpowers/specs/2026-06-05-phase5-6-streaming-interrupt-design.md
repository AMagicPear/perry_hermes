# Phase 5 + Phase 6 — Streaming & Interrupt

**Date:** 2026-06-05
**Status:** Implemented. Historical design note; current code is authoritative.
**Scope implemented:** `hermes-core`, `hermes-providers`, `hermes-loop`, `hermes-runtime`, `hermes-cli`
**Post-implementation note:** runtime was later made the shared CLI/gateway composition point, so older sections saying runtime was out of scope are stale.

## 1. Goals

- **Phase 5:** Token-level streaming from LLM → CLI. Assistant text appears as it is generated, not after the full completion arrives.
- **Phase 6:** Ctrl+C cleanly aborts an in-flight stream. Already-flushed content is preserved as a partial `Assistant` message in conversation history (so the next turn has the partial context, and a retry doesn't repeat the prefix).

These two phases are merged into a single round of work because Phase 5's `select!`-driven stream consumer is exactly the mechanism Phase 6 needs to wire Ctrl+C into.

## 2. Non-Goals

- No `--no-stream` flag. Streaming is always on. Providers that can't stream implement `stream()` as a single-delta yield (i.e. they emit one chunk and the stream ends).
- No TUI / ratatui work. Plain stderr printing only.
- No line-buffered rendering, table detection, code-block folding, or context scrubber. Python Hermes has all of these (20+ files in `hermes-agent/agent/`); porting them is later.
- No `IterationBudget`, no streaming retry, no per-token watchdog timers. Minimum viable cancellation only.
- No changes to `Content::Parts` handling (separate P1 from `hermes-comparison.md`).
- No Anthropic / Gemini provider — they get their own later rounds; they will follow the same `stream()` contract when added.

## 3. Architecture

### 3.1 Component diagram

```
┌──────────────────────┐
│ hermes-cli (REPL)    │  prints ContentDelta/ReasoningDelta deltas to stderr
│ run_repl on_event    │  accumulates ToolCallPartial into ToolCallStarted
└──────────┬───────────┘
           │ uses
           ▼
┌──────────────────────┐
│ hermes-loop          │  AgentLoop::run always calls provider.stream()
│ agent.rs             │  private accumulate_stream() drives deltas
│                      │  emits LoopEvent::ContentDelta/ReasoningDelta/ToolCallPartial
└──────────┬───────────┘
           │ uses
           ▼
┌──────────────────────┐
│ hermes-core          │  Provider trait — only required method is stream()
│ provider.rs          │  CompletionStream = Pin<Box<dyn Stream<Item=CompletionDelta>>>
│                      │  ToolCallDelta (new) for streaming tool_calls
│                      │  public accumulate_stream() helper (used by trait default)
└──────────┬───────────┘
           │ implemented by
           ▼
┌──────────────────────┐
│ hermes-providers     │  OpenAiProvider::stream — SSE parse + delta emit
│ openai.rs            │  EchoProvider::stream — yields one delta
│ echo.rs              │
└──────────────────────┘
```

### 3.2 Data flow (one turn, no tools)

1. CLI calls `AgentLoop::run(messages, ctx, cancel, on_event)`.
2. Loop calls `provider.stream(&messages, &tools, cancel)`, gets back a `CompletionStream`.
3. Loop drives the stream in a `tokio::select! { cancel.cancelled() | stream.next() }` loop:
   - Each `Some(Ok(delta))`:
     - `content_delta` → `on_event(ContentDelta(s))`, append to `acc.content`
     - `reasoning_delta` → `on_event(ReasoningDelta(s))`, append to `acc.reasoning`
     - `tool_call_delta` → `acc.add_tool_call_delta(td)`, `on_event(ToolCallPartial(td))`
     - `usage` → overwrite `acc.usage`
     - `finish_reason` → store in `acc.finish_reason`, **break**
   - `Some(Err(e))` → return `Err(LoopError::Provider(e))`. Already-emitted deltas are **lost** (network failure path — see §6).
   - `None` (stream ended without explicit `finish_reason`) → break; `acc.finalize()` defaults `finish_reason` to `Stop` in this case (see `StreamAccumulator::finalize` in §7.2).
4. On `cancel.cancelled()`:
   - Build a partial `Message { role: Assistant, content: Text(acc.content), tool_calls: <completed only>, reasoning: Some(acc.reasoning) }` (skip any `tool_call` whose accumulated `arguments` JSON is not valid — see §6.2).
   - If the partial is entirely empty (no content, no reasoning, no completed tool calls), return `Err(LoopError::Cancelled)`. Otherwise return `Err(LoopError::CancelledWith(partial))`. The empty check is the loop's job; the CLI never sees an empty partial.
5. On normal completion (`acc.finish_reason.is_some()` or stream ended `None`):
   - `let completion = acc.finalize()` — assembles `Message`, `Usage`, `FinishReason` from accumulated state.
   - Persist `completion.message` into `messages` history.
   - `on_event(AssistantMessage(clone))`.
   - React to `finish_reason` (existing logic, unchanged: `Stop`/`Length` returns, `ToolUse` dispatches tools, `ContentFilter`/`Error` returns errors).

### 3.3 Data flow (one turn, with tool calls, in streaming form)

OpenAI streams tool calls as a series of `tool_call_delta` chunks for a given `index`:

```
delta.tool_calls: [{ index: 0, id: "call_abc", function: { name: "bash", arguments: "" } }]
delta.tool_calls: [{ index: 0,                       function: {                   arguments: "{\"comman" } }]
delta.tool_calls: [{ index: 0,                       function: {                   arguments: "d\":\"ls\"}" } }]
delta.tool_calls: [{                                  finish_reason: "tool_calls" }]
```

`acc` maintains `BTreeMap<usize, ToolCall>` keyed by `index`. Each `ToolCallDelta` mutates the entry. When `finish_reason = ToolUse` arrives, `acc.finalize()` snapshots the map into `Vec<ToolCall>` (BTreeMap iterates in key order, so the output is sorted by `index` — important when the LLM emits multiple parallel calls and the user reads them top-to-bottom in the tool feed). The loop then dispatches them with the existing `dispatch_tool` logic (unchanged).

`ToolCallStarted` is emitted **once per completed tool call**, not per partial. This keeps the existing CLI tool-rendering unchanged.

## 4. Core types

### 4.1 `Provider` trait (in `hermes-core/src/provider.rs`)

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn model(&self) -> &str;

    /// Stream deltas as the LLM generates them. The consumer drives the
    /// stream to completion (or cancellation), emitting one event per
    /// delta, then assembles a final `Completion` from accumulated state.
    ///
    /// Cancellation contract: the consumer MUST `select!` on the cancel
    /// token and drop the stream when cancelled. Dropping the stream
    /// aborts the in-flight HTTP body.
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<CompletionStream, ProviderError>;

    /// Convenience: drive the stream to a single `Completion`. Default
    /// implementation uses `accumulate_stream`. Providers do not override.
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<Completion, ProviderError> {
        let stream = self.stream(messages, tools, cancel).await?;
        accumulate_stream(stream).await
    }
}
```

`accumulate_stream` is a public free function in the same module. It is a pure function over the stream — no provider-specific knowledge — so it lives in `hermes-core`, available to all `Provider` implementors.

### 4.2 `ToolCallDelta` (new, in `hermes-core/src/provider.rs`)

```rust
/// One chunk of a streaming tool call. OpenAI emits these incrementally:
/// the first chunk for a given `index` carries `id` and `name`; later
/// chunks carry `arguments_delta` (a JSON string fragment, NOT a parsed
/// value — the consumer concatenates them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments_delta: Option<String>,
}
```

`CompletionDelta` updates:

```rust
pub struct CompletionDelta {
    pub content_delta: Option<String>,
    pub reasoning_delta: Option<String>,
    pub tool_call_delta: Option<ToolCallDelta>,  // changed from Option<ToolCall>
    pub usage: Option<Usage>,                    // new — final chunk carries it
    pub finish_reason: Option<FinishReason>,
}
```

Breaking change: `tool_call_delta` type changes from `Option<ToolCall>` to `Option<ToolCallDelta>`. No other code currently constructs `CompletionDelta` (it was just a type definition), so the impact is contained to the provider + loop.

### 4.3 `LoopEvent` (in `hermes-loop/src/agent.rs`)

Three new variants:

```rust
pub enum LoopEvent {
    Thinking,
    ContentDelta(String),       // text token from streaming assistant
    ReasoningDelta(String),     // reasoning token (o1, extended thinking)
    ToolCallPartial(ToolCallDelta),  // silent-accumulated; ToolCallStarted fires when complete
    AssistantMessage(Message),
    ToolCallStarted { call: ToolCall, iteration: u32 },
    ToolCallFinished { call: ToolCall, result: Result<ToolOutput, hermes_core::error::ToolError> },
    LengthLimit,
    IterationsExhausted,
    Cancelled,
}
```

`ToolCallStarted` continues to be emitted only when a tool call is **complete** (i.e. when the stream hits `finish_reason = tool_calls`). This is what existing CLI rendering and tests expect.

### 4.4 `LoopError` (in `hermes-core/src/error.rs`)

New variant:

```rust
pub enum LoopError {
    MaxIterations(u32),
    Timeout(std::time::Duration),
    Cancelled,
    CancelledWith(Message),  // new: cancellation that produced a usable partial message
    ContentFilter,
    Provider(#[from] ProviderError),
    Compression(String),
}
```

`Cancelled` is kept for cases where there is nothing to preserve (e.g. cancel arrived before the stream produced any content). `CancelledWith` carries the partial.

## 5. Provider implementations

### 5.1 `OpenAiProvider::stream` (in `hermes-providers/src/openai.rs`)

The existing `complete()` is deleted. `stream()` is the only method.

**Request shape:** same as before, plus `"stream": true` in the JSON body. The existing `build_request` helper (extracted from the old `complete()`) is reused.

**Byte stream:** `reqwest::Response::bytes_stream()` → `impl Stream<Item = reqwest::Result<Bytes>>`.

**SSE parser:** a private free function

```rust
fn parse_sse_chunks(
    bytes: impl futures::Stream<Item = reqwest::Result<Bytes>> + Unpin,
    cancel: CancellationToken,
) -> impl futures::Stream<Item = Result<CompletionDelta, ProviderError>>
```

Behavior:

- Maintain an internal buffer of unparsed bytes.
- On each new chunk, append to buffer; scan for `\n\n` (SSE event boundary).
- For each complete event:
  - Split into lines.
  - Skip lines starting with `:` (SSE comments).
  - Lines starting with `data: ` are payload. Strip the prefix, trim.
    - If payload is exactly `[DONE]` → end the stream.
    - Otherwise parse as JSON: `{ choices: [{ delta: { content?, reasoning_content?, tool_calls? }, finish_reason? }], usage? }`.
      - Map fields to `CompletionDelta`. (See §4.2.)
- Cancellation: the outer `tokio::select!` in the loop (§3.2 step 3) handles this. Inside the SSE parser, just stop yielding.

**HTTP errors:** the existing pre-stream status check (401/429/non-2xx) stays. Once we are inside the stream, mid-stream errors are `reqwest` decode failures that propagate through the byte stream as `Some(Err(reqwest::Error))`, which `parse_sse_chunks` converts into `ProviderError::Transport` / `ProviderError::InvalidResponse`.

**Timeouts:** the existing 120s client timeout applies. No per-stream watchdog in this round.

### 5.2 `EchoProvider::stream` (in `hermes-providers/src/echo.rs`)

The existing `complete()` is deleted. `stream()` is implemented as:

```rust
async fn stream(&self, messages, _tools, cancel) -> Result<CompletionStream, ProviderError> {
    if cancel.is_cancelled() { return Err(ProviderError::Cancelled); }
    let last_user = /* existing logic */;
    let reply = format!("echo: {}", /* existing */);
    let delta = CompletionDelta {
        content_delta: Some(reply),
        reasoning_delta: None,
        tool_call_delta: None,
        usage: Some(Usage::default()),
        finish_reason: Some(FinishReason::Stop),
    };
    Ok(Box::pin(futures::stream::once(async move { Ok(delta) })))
}
```

One delta, `Stop`, done. The existing test `echo_provider_runs_one_iteration_and_stops` continues to pass — it just exercises the new `stream` path through the default `complete`.

## 6. Error handling

### 6.1 Cancellation (Ctrl+C during stream)

- The user presses Ctrl+C. The persistent listener in `hermes-cli/src/main.rs` (added in the previous round) calls `cancel.cancel()`.
- Inside `AgentLoop::run`, the `tokio::select!` sees the cancelled branch.
- Build a partial `Message`:
  - `role = Assistant`
  - `content = Text(acc.content)` (already-flushed text)
  - `reasoning = Some(acc.reasoning)` if non-empty, else `None`
  - `tool_calls`: include only calls whose accumulated `arguments` JSON parses successfully. Discard any half-built call. (See §6.2.)
- If the partial is entirely empty (no content, no reasoning, no completed tool calls), return `Err(LoopError::Cancelled)` — there is nothing to preserve, the CLI shows `[cancelled]` and pops the unprocessed user message. Otherwise return `Err(LoopError::CancelledWith(partial))`.
- CLI handles the non-empty case: append `partial` to `history`, print a one-line notice like `[cancelled mid-stream: 142 chars streamed, 1 tool call kept]`, return to prompt. The exact format is decided in implementation — the spec only requires a count to appear.

### 6.2 Partial tool calls on cancel

- A tool call is "complete enough to keep" iff `acc.tool_calls[index].arguments` is a JSON value (any value, including `null`). Incomplete calls are dropped.
- Rationale: a partial JSON fragment cannot be passed to a tool. LLM does not need it (it never sent the call). Keeping it would just be noise in history.

### 6.3 Mid-stream network failure

- `stream.next()` returns `Some(Err(reqwest::Error))` or a parser error.
- Loop returns `Err(LoopError::Provider(...))`.
- **Already-emitted deltas are discarded** — not pushed into `history`. The user sees `error: ...` and the prompt returns to clean state.
- This differs from cancel (§6.1) by intent: a network failure means we don't know if the model produced something semantically valid; a cancel is deliberate and the user can see exactly what was streamed.

### 6.4 HTTP 4xx/5xx

- Detected at `Response::status()` before entering the stream.
- Existing variants: `ProviderError::Auth`, `ProviderError::RateLimited`, `ProviderError::InvalidResponse` (for body text).
- No stream is constructed. No partial content to discard.

### 6.5 SSE protocol error (malformed JSON inside a `data:` line)

- Caught by `parse_sse_chunks`, yielded as `Some(Err(ProviderError::InvalidResponse))`.
- Same as §6.3: discard deltas, return error.

## 7. AgentLoop changes

### 7.1 Private `accumulate_stream` (in `hermes-core/src/provider.rs`)

This is the public helper that the trait default `complete()` calls. It is also reused inside `AgentLoop::run` after `provider.stream()` returns, **with one twist**: the loop's accumulation is interleaved with `on_event` calls, so the loop has its own private version that takes a callback.

```rust
// In hermes-core — the public, callback-less version:
pub async fn accumulate_stream(
    mut stream: CompletionStream,
) -> Result<Completion, ProviderError> {
    let mut acc = StreamAccumulator::new();
    while let Some(delta) = stream.next().await {
        let delta = delta?;
        acc.add(&delta);
        if delta.finish_reason.is_some() { break; }
    }
    Ok(acc.finalize())
}
```

The loop has its own `fn drive_stream_with_callback(...)` in `hermes-loop/src/agent.rs` that mirrors this but also calls `on_event` for each delta. The two share a `StreamAccumulator` struct that lives in `hermes-core` (it's pure data).

### 7.2 `StreamAccumulator` (in `hermes-core/src/provider.rs`)

```rust
pub struct StreamAccumulator {
    content: String,
    reasoning: String,
    tool_calls: BTreeMap<usize, ToolCall>,  // BTreeMap so finalize() yields sorted Vec
    usage: Usage,
    finish_reason: Option<FinishReason>,
}

impl StreamAccumulator {
    pub fn new() -> Self;
    pub fn add(&mut self, delta: &CompletionDelta);
    /// Build the final `Completion`. If `finish_reason` was never set
    /// (stream ended with `None`), defaults to `FinishReason::Stop`.
    pub fn finalize(self) -> Completion;
    pub fn content(&self) -> &str;
    pub fn reasoning(&self) -> &str;
    pub fn tool_calls(&self) -> Vec<ToolCall>;
    /// True if the accumulator has produced nothing (no content, no
    /// reasoning, no completed tool calls). Used by the loop to decide
    /// between `Cancelled` and `CancelledWith`.
    pub fn is_empty(&self) -> bool;
    /// Build a `Message` for the cancellation path. Filters tool calls
    /// by JSON-parse validity (see §6.2). Caller checks `is_empty()`
    /// before deciding to push this into history.
    pub fn into_partial_message(self, role: Role) -> Message;
    pub fn finish_reason(&self) -> Option<FinishReason>;
}
```

`into_partial_message` filters tool calls by JSON parse validity (see §6.2).

### 7.3 `AgentLoop::run` rewrite

The body of the per-iteration LLM call changes from:

```rust
let completion = self.provider.complete(&messages, &tools, cancel.clone()).await?;
```

to:

```rust
let stream = self.provider.stream(&messages, &tools, cancel.clone()).await?;
let mut stream = stream;
let mut acc = StreamAccumulator::new();
loop {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            on_event(LoopEvent::Cancelled);
            return if acc.is_empty() {
                Err(LoopError::Cancelled)
            } else {
                Err(LoopError::CancelledWith(acc.into_partial_message(Role::Assistant)))
            };
        }
        chunk = stream.next() => {
            match chunk {
                Some(Ok(delta)) => {
                    if let Some(s) = &delta.content_delta {
                        on_event(LoopEvent::ContentDelta(s.clone()));
                    }
                    if let Some(s) = &delta.reasoning_delta {
                        on_event(LoopEvent::ReasoningDelta(s.clone()));
                    }
                    if let Some(td) = &delta.tool_call_delta {
                        on_event(LoopEvent::ToolCallPartial(td.clone()));
                    }
                    acc.add(&delta);
                    if delta.finish_reason.is_some() { break; }
                }
                Some(Err(e)) => return Err(LoopError::Provider(e)),
                None => break,
            }
        }
    }
}
let completion = acc.finalize();
```

Everything after this point (metrics increment, message persist, `on_event(AssistantMessage)`, finish-reason match) is unchanged.

## 8. CLI rendering

The `on_event` closure in `run_repl` (in `hermes-cli/src/main.rs`) gets three new match arms. `Cancelled` keeps its existing behavior. `CancelledWith` is handled in the `Err` branch of the post-run `match` (it pushes the partial into history).

`…` spinner is replaced on first `ContentDelta` because we directly `eprint!` the token without a leading newline. The "thinking" state is short-lived and that is fine.

Reasoning deltas render in dim ANSI: `"\x1b[2m{s}\x1b[0m"`. When stderr is not a TTY (e.g. piped), the escape codes still appear but are harmless.

## 9. Testing

### 9.1 `hermes-core`

- `accumulate_stream` unit tests:
  - Single-delta stream with `Stop` produces a `Completion` with the expected message + `Usage`.
  - Multi-delta content concatenation.
  - Multi-delta reasoning concatenation (separate from content).
  - Multiple tool calls with different `index` values interleave correctly.
  - Stream ending with `None` (no explicit `finish_reason`) is treated as `Stop`.
  - Stream yielding `Some(Err(_))` propagates the error.

### 9.2 `hermes-loop`

- `StreamedMockProvider` in `hermes-loop/tests/streamed_mock.rs`: configurable sequence of `CompletionDelta`s.
  - Replaces the existing `ScriptedProvider` (which is removed). The existing scripted tests are rewritten in terms of the stream API: each scripted `Completion` becomes a small sequence of `CompletionDelta`s that the mock yields.
- Integration tests:
  - Single-stop streaming turn: assert `ContentDelta` events fire in order, `AssistantMessage` fires once at the end, metrics show `iterations=1`.
  - Tool-use streaming turn: assert `ToolCallPartial` events fire, then `ToolCallStarted` fires once with the assembled call, then `ToolCallFinished`, then the next iteration starts.
  - Cancel mid-stream: assert `CancelledWith` carries the partial content, `Cancelled` event fires, history contains the partial.

### 9.3 `hermes-providers`

- `OpenAiProvider::stream` integration test using `tokio::net::TcpListener` (per CLAUDE.md, httpmock 0.7 doesn't expose captured bodies — we need raw byte control).
  - The test server sends canned SSE bytes (multiple events + `[DONE]`).
  - Assert the returned stream yields the expected `CompletionDelta` sequence.
  - Variant: server sends malformed JSON in one event → assert the stream yields `Some(Err(InvalidResponse))`.
  - Variant: server hangs without sending `[DONE]`; the test fires `cancel` and asserts the stream ends (caller drops it).

### 9.4 `hermes-cli`

- End-to-end test using `StreamedMockProvider`:
  - Pipe `"hello\n/exit\n"` into the binary, run with `--provider streamed` (a new test-only provider added behind `#[cfg(test)]` or behind a feature flag).
  - Assert stderr contains the streamed text and the `[iterations=...]` line.
  - If wiring a test-only provider into the binary is awkward, this test is omitted; the `hermes-loop` integration tests are the binding constraint.

## 10. Migration & rollout

- No external users; this is an internal refactor. The breaking change to `CompletionDelta::tool_call_delta` (type change) and the deletion of `Provider::complete` from implementors' required methods is contained to the workspace.
- `OpenAiProvider` and `EchoProvider` are the only two implementors. Both are updated in this round.
- `hermes-runtime::AIAgent::run_turn` continues to work — it goes through `Provider::stream` (or the default `complete` if it calls that), and the public API is unchanged.

## 11. Open follow-ups (out of scope for this round)

- Anthropic / Gemini / Bedrock providers all implement the same `stream()` contract.
- `--no-stream` flag (forces non-streaming path; useful for CI logs).
- TUI rendering of streaming tokens (ratatui, color, spinner replacement).
- Context scrubber, table detection, code-block folding (Python Hermes features).
- `IterationBudget` (refund / grace call / subagent).
- `parallel_tool_calls = true` plumbing with stream.
