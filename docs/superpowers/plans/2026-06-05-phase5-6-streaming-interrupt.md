# Phase 5 + Phase 6 — Streaming & Interrupt Implementation Plan

> **Status:** Implemented. Historical execution plan; check current code before following exact API snippets.
>
> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `AgentLoop::run` always consume a streaming LLM response, emit per-token events to the CLI, and cleanly cancel on Ctrl+C with partial content preserved.

**Architecture:** `Provider::stream()` becomes the only required method on the trait; `complete()` is a default impl that drives `accumulate_stream`. The loop's LLM call site becomes a `tokio::select!` over the stream + cancel token. The loop accumulates deltas into a `StreamAccumulator` (pure data, lives in core), emits `LoopEvent::ContentDelta` / `ReasoningDelta` / `ToolCallPartial`, and assembles a final `Completion` on stream end.

**Tech Stack:** Rust 1.75+, `tokio`, `tokio_util::sync::CancellationToken`, `futures` (StreamExt), `reqwest` (bytes_stream), `serde_json`. No new dependencies.

**Spec:** [`docs/superpowers/specs/2026-06-05-phase5-6-streaming-interrupt-design.md`](../specs/2026-06-05-phase5-6-streaming-interrupt-design.md)

---

## File Structure

**Modified files:**
- `crates/hermes-core/src/provider.rs` — `CompletionDelta`, `ToolCallDelta` (new), `Provider` trait, `StreamAccumulator` (new), `accumulate_stream` (new)
- `crates/hermes-core/src/error.rs` — `LoopError::CancelledWith(Message)` (new variant)
- `crates/hermes-core/src/lib.rs` — re-exports
- `crates/hermes-providers/src/echo.rs` — implement `stream()` only
- `crates/hermes-providers/src/openai.rs` — implement `stream()` with SSE parser; delete `complete()`
- `crates/hermes-loop/src/agent.rs` — add `LoopEvent` variants, rewrite `run()` to use stream
- `crates/hermes-loop/src/lib.rs` — re-exports
- `crates/hermes-cli/src/main.rs` — `on_event` arms for new variants, `CancelledWith` history push
- `crates/hermes-loop/tests/arg_validation.rs` — update `ScriptedProvider` to implement `stream` only
- `crates/hermes-loop/tests/tool_dispatch.rs` — same

**New files:**
- `crates/hermes-providers/tests/openai_stream.rs` — integration test with `tokio::net::TcpListener` (per CLAUDE.md, httpmock doesn't expose captured bodies)

**No new dependencies.** `futures::stream` and `reqwest::Response::bytes_stream` are already available transitively.

---

## Task Dependency Graph

```
T1 (Core: ToolCallDelta + CompletionDelta)  ──┐
T2 (Core: StreamAccumulator)                 ──┤
T3 (Core: accumulate_stream)                 ──┤
T4 (Core: Provider trait refactor)           ──┤
T5 (Core: LoopError::CancelledWith)          ──┘
                                                │
                                                ▼
T6 (Providers: EchoProvider::stream)        ──┐
T7 (Providers: OpenAiProvider::stream)       ──┤
T8 (Providers: OpenAI SSE test)              ──┘
                                                │
                                                ▼
T9 (Loop: new LoopEvent variants)           ──┐
T10 (Loop: AgentLoop::run rewrite)          ──┤
T11 (Loop: ScriptedProvider updates)        ──┘
                                                │
                                                ▼
T12 (CLI: on_event + CancelledWith)
T13 (Verify: cargo test + clippy + smoke)
```

T1–T5 are core refactors and must be done first (the trait change cascades). T6–T8 unblock the providers. T9–T11 are loop-side. T12 is CLI. T13 is full verification.

T1, T2, T3 are independent of each other (different types) and can be parallelized via subagents if desired. T4 must come after T1 (it uses the new delta type). T5 is independent.

---

## Task 1: Add `ToolCallDelta` and change `CompletionDelta::tool_call_delta` type

**Files:**
- Modify: `crates/hermes-core/src/provider.rs:88-94`

`CompletionDelta::tool_call_delta` is currently `Option<ToolCall>`, which is wrong for streaming (the JSON arrives in fragments). Change to `Option<ToolCallDelta>`.

- [ ] **Step 1: Edit `crates/hermes-core/src/provider.rs`**

Replace the `CompletionDelta` struct block and add `ToolCallDelta` above it:

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

/// One chunk of a streaming response.
#[derive(Debug, Clone)]
pub struct CompletionDelta {
    pub content_delta: Option<String>,
    pub reasoning_delta: Option<String>,
    pub tool_call_delta: Option<ToolCallDelta>,
    pub usage: Option<Usage>,
    pub finish_reason: Option<FinishReason>,
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p hermes-core`
Expected: errors in `hermes-providers` and `hermes-loop` (they still reference the old type). Do NOT fix them yet — that's later tasks. Confirm the error is the expected one: `error[E0308]: mismatched types — expected ToolCallDelta, found ToolCall`.

Run: `cargo build -p hermes-core 2>&1 | grep -E "error\[E" | head -5`
Expected output: errors only in `hermes-providers` / `hermes-loop` crates, not in `hermes-core`.

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-core/src/provider.rs
git commit -m "feat(core): add ToolCallDelta, change CompletionDelta::tool_call_delta type"
```

---

## Task 2: Add `StreamAccumulator` (pure data, no I/O)

**Files:**
- Modify: `crates/hermes-core/src/provider.rs` (add at the end, after the `FinishReason` impl)

This is the data struct that holds in-progress stream state. No I/O, no async — just methods that mutate based on a delta and produce a final `Completion`.

- [ ] **Step 1: Add `StreamAccumulator` struct and impl**

Append to `crates/hermes-core/src/provider.rs`:

```rust
use std::collections::BTreeMap;
use crate::message::{Message, Role, ToolCall};
use crate::usage::Usage;

/// Accumulates `CompletionDelta` items from a stream into a final `Completion`.
///
/// Pure data — no async, no I/O. Lives in `hermes-core` so both the trait
/// default `complete()` and `AgentLoop::run` can share it.
pub struct StreamAccumulator {
    content: String,
    reasoning: String,
    tool_calls: BTreeMap<usize, ToolCall>,
    usage: Usage,
    finish_reason: Option<FinishReason>,
}

impl Default for StreamAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamAccumulator {
    pub fn new() -> Self {
        Self {
            content: String::new(),
            reasoning: String::new(),
            tool_calls: BTreeMap::new(),
            usage: Usage::default(),
            finish_reason: None,
        }
    }

    pub fn add(&mut self, delta: &CompletionDelta) {
        if let Some(s) = &delta.content_delta {
            self.content.push_str(s);
        }
        if let Some(s) = &delta.reasoning_delta {
            self.reasoning.push_str(s);
        }
        if let Some(td) = &delta.tool_call_delta {
            let entry = self.tool_calls.entry(td.index).or_insert_with(|| ToolCall {
                id: String::new(),
                name: String::new(),
                arguments: serde_json::Value::Null,
            });
            if let Some(id) = &td.id {
                entry.id = id.clone();
            }
            if let Some(name) = &td.name {
                entry.name = name.clone();
            }
            if let Some(args_frag) = &td.arguments_delta {
                // Concatenate fragments into a raw JSON string. We do not
                // parse incrementally — only `into_partial_message` and
                // `finalize` attempt to parse, and only over the full string.
                let existing = match &entry.arguments {
                    serde_json::Value::Null => String::new(),
                    serde_json::Value::String(s) => s.clone(),
                    v => v.to_string(),
                };
                let combined = format!("{existing}{args_frag}");
                entry.arguments = serde_json::Value::String(combined);
            }
        }
        if let Some(u) = delta.usage {
            self.usage = u;
        }
        if let Some(fr) = delta.finish_reason {
            self.finish_reason = Some(fr);
        }
    }

    /// Build the final `Completion`. If `finish_reason` was never set
    /// (stream ended with `None`), defaults to `FinishReason::Stop`.
    pub fn finalize(mut self) -> Completion {
        // Parse any accumulated tool-call arguments strings into JSON.
        for tc in self.tool_calls.values_mut() {
            if let serde_json::Value::String(s) = &tc.arguments {
                if let Ok(parsed) = serde_json::from_str(s) {
                    tc.arguments = parsed;
                }
            }
        }
        let finish_reason = self.finish_reason.unwrap_or(FinishReason::Stop);
        let tool_calls = if self.tool_calls.is_empty() {
            None
        } else {
            Some(self.tool_calls.into_values().collect())
        };
        let message = Message {
            role: Role::Assistant,
            content: Content::Text(std::mem::take(&mut self.content)),
            reasoning: if self.reasoning.is_empty() {
                None
            } else {
                Some(std::mem::take(&mut self.reasoning))
            },
            tool_call_id: None,
            tool_calls,
        };
        Completion {
            message,
            usage: self.usage,
            finish_reason,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
            && self.reasoning.is_empty()
            && self.tool_calls.is_empty()
    }

    /// Build a `Message` for the cancellation path. Filters out tool calls
    /// whose accumulated arguments are not valid JSON. Caller checks
    /// `is_empty()` before deciding to push into history.
    pub fn into_partial_message(mut self, role: Role) -> Message {
        self.tool_calls.retain(|_, tc| {
            if let serde_json::Value::String(s) = &tc.arguments {
                serde_json::from_str::<serde_json::Value>(s).is_ok()
            } else {
                true
            }
        });
        let tool_calls = if self.tool_calls.is_empty() {
            None
        } else {
            Some(self.tool_calls.into_values().collect())
        };
        Message {
            role,
            content: Content::Text(self.content),
            reasoning: if self.reasoning.is_empty() {
                None
            } else {
                Some(self.reasoning)
            },
            tool_call_id: None,
            tool_calls,
        }
    }
}
```

Add to the existing `use` block at the top of the file:
```rust
use crate::message::{Content, Message, Role, ToolCall};
```

(Or extend if `Content` is already imported — check first. `Message` is already imported.)

- [ ] **Step 2: Write the failing test in `crates/hermes-core/src/provider.rs` `#[cfg(test)] mod tests`**

If there's no test module yet, add it at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Content;

    fn delta(content: Option<&str>, reasoning: Option<&str>) -> CompletionDelta {
        CompletionDelta {
            content_delta: content.map(String::from),
            reasoning_delta: reasoning.map(String::from),
            tool_call_delta: None,
            usage: None,
            finish_reason: None,
        }
    }

    #[test]
    fn empty_accumulator_finalizes_as_stop() {
        let mut acc = StreamAccumulator::new();
        acc.add(&delta(None, None));
        let completion = acc.finalize();
        assert_eq!(completion.finish_reason, FinishReason::Stop);
        assert!(matches!(completion.message.content, Content::Text(ref s) if s.is_empty()));
        assert!(completion.message.tool_calls.is_none());
    }

    #[test]
    fn content_concatenates_across_deltas() {
        let mut acc = StreamAccumulator::new();
        acc.add(&delta(Some("Hel"), None));
        acc.add(&delta(Some("lo "), None));
        acc.add(&delta(Some("world"), Some(None)));
        acc.add(&delta(None, Some("thinking...")));
        let completion = acc.finalize();
        assert!(matches!(completion.message.content, Content::Text(ref s) if s == "Hello world"));
        assert_eq!(completion.message.reasoning.as_deref(), Some("thinking..."));
    }

    #[test]
    fn tool_calls_accumulate_by_index() {
        let mut acc = StreamAccumulator::new();
        acc.add(&CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: Some(ToolCallDelta {
                index: 0,
                id: Some("call_a".into()),
                name: Some("bash".into()),
                arguments_delta: Some("{\"command\":".into()),
            }),
            usage: None,
            finish_reason: None,
        });
        acc.add(&CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: Some(ToolCallDelta {
                index: 0,
                id: None,
                name: None,
                arguments_delta: Some("\"ls\"}".into()),
            }),
            usage: None,
            finish_reason: None,
        });
        acc.add(&CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: Some(ToolCallDelta {
                index: 1,
                id: Some("call_b".into()),
                name: Some("bash".into()),
                arguments_delta: Some("{\"command\":\"pwd\"}".into()),
            }),
            usage: None,
            finish_reason: Some(FinishReason::ToolUse),
        });
        let completion = acc.finalize();
        assert_eq!(completion.finish_reason, FinishReason::ToolUse);
        let calls = completion.message.tool_calls.expect("two tool calls");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_a");
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[0].arguments, serde_json::json!({"command": "ls"}));
        assert_eq!(calls[1].id, "call_b");
        assert_eq!(calls[1].arguments, serde_json::json!({"command": "pwd"}));
    }

    #[test]
    fn is_empty_true_for_no_deltas() {
        let acc = StreamAccumulator::new();
        assert!(acc.is_empty());
    }

    #[test]
    fn is_empty_false_after_content_delta() {
        let mut acc = StreamAccumulator::new();
        acc.add(&delta(Some("hi"), None));
        assert!(!acc.is_empty());
    }

    #[test]
    fn into_partial_message_drops_incomplete_tool_calls() {
        let mut acc = StreamAccumulator::new();
        acc.add(&delta(Some("text so far"), None));
        // Complete call — should be kept
        acc.add(&CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: Some(ToolCallDelta {
                index: 0,
                id: Some("call_a".into()),
                name: Some("bash".into()),
                arguments_delta: Some("{\"command\":\"ls\"}".into()),
            }),
            usage: None,
            finish_reason: None,
        });
        // Incomplete call — args is "{\"comm" which is not valid JSON
        acc.add(&CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: Some(ToolCallDelta {
                index: 1,
                id: Some("call_b".into()),
                name: Some("bash".into()),
                arguments_delta: Some("{\"comm".into()),
            }),
            usage: None,
            finish_reason: None,
        });
        let partial = acc.into_partial_message(Role::Assistant);
        let calls = partial.tool_calls.expect("one complete call kept");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_a");
    }
}
```

- [ ] **Step 3: Run the tests, confirm they pass**

Run: `cargo test -p hermes-core --lib`
Expected: all 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/hermes-core/src/provider.rs
git commit -m "feat(core): add StreamAccumulator with full TDD coverage"
```

---

## Task 3: Add `accumulate_stream` public function

**Files:**
- Modify: `crates/hermes-core/src/provider.rs` (append after `StreamAccumulator`)

A pure function that drives a `CompletionStream` to completion and returns the final `Completion`. Used by the trait default `complete()`.

- [ ] **Step 1: Add the function**

```rust
use futures::StreamExt;

/// Drive a `CompletionStream` to completion and return the final `Completion`.
///
/// This is a public helper used by the default `Provider::complete` impl.
/// It does NOT emit per-delta events — for that, use `AgentLoop::run` which
/// has its own private drive loop.
pub async fn accumulate_stream(
    mut stream: CompletionStream,
) -> Result<Completion, ProviderError> {
    let mut acc = StreamAccumulator::new();
    while let Some(item) = stream.next().await {
        let delta = item?;
        acc.add(&delta);
        if delta.finish_reason.is_some() {
            break;
        }
    }
    Ok(acc.finalize())
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p hermes-core`
Expected: compiles cleanly.

- [ ] **Step 3: Add a unit test**

Append to the existing `#[cfg(test)] mod tests` in `provider.rs`:

```rust
    #[tokio::test]
    async fn accumulate_stream_returns_completion() {
        use futures::stream;
        let deltas = vec![
            Ok(CompletionDelta {
                content_delta: Some("Hello".into()),
                reasoning_delta: None,
                tool_call_delta: None,
                usage: None,
                finish_reason: None,
            }),
            Ok(CompletionDelta {
                content_delta: Some(" world".into()),
                reasoning_delta: None,
                tool_call_delta: None,
                usage: Some(Usage { input_tokens: 1, output_tokens: 2, cached_input_tokens: 0 }),
                finish_reason: Some(FinishReason::Stop),
            }),
        ];
        let stream: CompletionStream = Box::pin(stream::iter(deltas));
        let completion = accumulate_stream(stream).await.unwrap();
        assert_eq!(completion.finish_reason, FinishReason::Stop);
        assert!(matches!(completion.message.content, Content::Text(ref s) if s == "Hello world"));
        assert_eq!(completion.usage.output_tokens, 2);
    }

    #[tokio::test]
    async fn accumulate_stream_defaults_to_stop_on_none() {
        use futures::stream;
        let deltas = vec![Ok(CompletionDelta {
            content_delta: Some("hi".into()),
            reasoning_delta: None,
            tool_call_delta: None,
            usage: None,
            finish_reason: None,
        })];
        let stream: CompletionStream = Box::pin(stream::iter(deltas));
        let completion = accumulate_stream(stream).await.unwrap();
        assert_eq!(completion.finish_reason, FinishReason::Stop);
    }

    #[tokio::test]
    async fn accumulate_stream_propagates_error() {
        use futures::stream;
        let deltas: Vec<Result<CompletionDelta, ProviderError>> = vec![
            Ok(CompletionDelta {
                content_delta: Some("a".into()),
                reasoning_delta: None,
                tool_call_delta: None,
                usage: None,
                finish_reason: None,
            }),
            Err(ProviderError::InvalidResponse("boom".into())),
        ];
        let stream: CompletionStream = Box::pin(stream::iter(deltas));
        let result = accumulate_stream(stream).await;
        assert!(matches!(result, Err(ProviderError::InvalidResponse(_))));
    }
```

- [ ] **Step 4: Run, commit**

Run: `cargo test -p hermes-core --lib`
Expected: 9 tests pass (6 from Task 2 + 3 new).

```bash
git add crates/hermes-core/src/provider.rs
git commit -m "feat(core): add accumulate_stream public helper"
```

---

## Task 4: Refactor `Provider` trait — `stream` required, `complete` default

**Files:**
- Modify: `crates/hermes-core/src/provider.rs:14-41`

Make `stream()` the only required method. `complete()` becomes a default impl that calls `stream` + `accumulate_stream`.

- [ ] **Step 1: Rewrite the trait**

Replace the trait block (lines 14–41 of `provider.rs`) with:

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

- [ ] **Step 2: Verify it compiles cleanly in core, breaks in providers/loop**

Run: `cargo build -p hermes-core`
Expected: clean.

Run: `cargo build --workspace 2>&1 | grep -E "^error" | head -20`
Expected: errors in `hermes-providers` (EchoProvider, OpenAiProvider) and `hermes-loop/tests/` (ScriptedProvider) because they implement `complete` but not `stream`. We fix those in later tasks.

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-core/src/provider.rs
git commit -m "refactor(core): make Provider::stream the required method"
```

---

## Task 5: Add `LoopError::CancelledWith(Message)`

**Files:**
- Modify: `crates/hermes-core/src/error.rs`
- Modify: `crates/hermes-core/src/lib.rs` (re-exports — verify `Message` is reachable)

- [ ] **Step 1: Add the variant**

In `crates/hermes-core/src/error.rs`, add `use crate::message::Message;` at the top (or extend existing import) and add the variant to `LoopError`:

```rust
use crate::message::Message;

#[derive(Debug, Error)]
pub enum LoopError {
    #[error("max iterations ({0}) reached")]
    MaxIterations(u32),
    #[error("timeout after {0:?}")]
    Timeout(std::time::Duration),
    #[error("cancelled")]
    Cancelled,
    #[error("cancelled with partial response")]
    CancelledWith(Message),
    #[error("content filter triggered")]
    ContentFilter,
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),
    #[error("context compression failed: {0}")]
    Compression(String),
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p hermes-core`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-core/src/error.rs
git commit -m "feat(core): add LoopError::CancelledWith(Message) variant"
```

---

## Task 6: Rewrite `EchoProvider` to implement only `stream`

**Files:**
- Modify: `crates/hermes-providers/src/echo.rs`

`EchoProvider::complete` is deleted. `stream` yields a single delta and ends.

- [ ] **Step 1: Rewrite `echo.rs`**

Read the existing file first, then replace the entire content with:

```rust
//! Echo provider — yields a single "echo: <text>" delta and stops.
//! Useful for offline smoke tests of the agent loop without an API key.

use async_trait::async_trait;
use futures::stream;
use hermes_core::{
    message::{Content, Message, Role},
    provider::{CompletionDelta, CompletionStream, FinishReason, Provider},
    registry::ToolSchema,
    ProviderError, Usage,
};
use tokio_util::sync::CancellationToken;

pub struct EchoProvider {
    name: String,
    model: String,
}

impl EchoProvider {
    pub fn new() -> Self {
        Self { name: "echo".into(), model: "echo-v0".into() }
    }
}

impl Default for EchoProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for EchoProvider {
    fn name(&self) -> &str { &self.name }
    fn model(&self) -> &str { &self.model }

    async fn stream(
        &self,
        messages: &[Message],
        _tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<CompletionStream, ProviderError> {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let last_user = messages.iter().rev()
            .find(|m| m.role == Role::User)
            .cloned()
            .unwrap_or(Message {
                role: Role::Assistant,
                content: Content::Text("(nothing to echo)".into()),
                reasoning: None,
                tool_call_id: None,
                tool_calls: None,
            });
        let reply = match &last_user.content {
            Content::Text(t) => format!("echo: {t}"),
            Content::Parts(_) => "echo: (multimodal)".to_string(),
        };
        let delta = CompletionDelta {
            content_delta: Some(reply),
            reasoning_delta: None,
            tool_call_delta: None,
            usage: Some(Usage::default()),
            finish_reason: Some(FinishReason::Stop),
        };
        Ok(Box::pin(stream::once(async move { Ok(delta) })))
    }
}
```

- [ ] **Step 2: Verify only Echo is fixed; OpenAI / loop still broken**

Run: `cargo build -p hermes-providers 2>&1 | grep -E "^error" | head -5`
Expected: `OpenAiProvider` still has errors (only `complete`, no `stream`). That's fine — Task 7 fixes it.

- [ ] **Step 3: Run Echo test**

Run: `cargo test -p hermes-loop --test echo_loop`
Expected: PASS (uses the default `complete` impl which goes through `stream`).

- [ ] **Step 4: Commit**

```bash
git add crates/hermes-providers/src/echo.rs
git commit -m "refactor(providers): EchoProvider implements stream only"
```

---

## Task 7: Implement `OpenAiProvider::stream` with SSE parser

**Files:**
- Modify: `crates/hermes-providers/src/openai.rs`

Delete the existing `complete()`. Implement `stream()` that:
1. Sends the chat completion request with `"stream": true`.
2. Pre-flight: check `Response::status()` for 401/429/non-2xx (existing behavior, but as the first step before entering the SSE parser).
3. Returns `Box::pin(parse_sse_chunks(response.bytes_stream(), cancel))`.

The SSE parser is a new private function that:
- Buffers incoming bytes.
- Splits on `\n\n` (event boundary).
- For each `data: ` line, parses JSON, extracts `choices[0].delta`, maps to `CompletionDelta`.
- Stops on `[DONE]`.
- Yields `Some(Err(InvalidResponse))` on malformed JSON.

The existing `openai_compatible_request_body` logic from `complete()` is extracted into a helper `build_request_body` reused by `stream()`.

- [ ] **Step 1: Write a unit test for the SSE parser (TDD red)**

The parser is a free function that takes `Vec<u8>` of raw SSE bytes and returns `Vec<CompletionDelta>`. Test it without any network — just feed canned bytes.

Add to `crates/hermes-providers/src/openai.rs` (or in a `#[cfg(test)] mod tests` at the bottom of the file):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::CompletionDelta;
    use hermes_core::provider::ToolCallDelta;

    fn parse_sse_bytes(input: &[u8]) -> Result<Vec<CompletionDelta>, ProviderError> {
        // The parser is a private fn — make it pub(crate) for testing
        // OR expose via a `pub(crate) fn parse_sse_for_test(input: &[u8])` shim.
        parse_sse_for_test(input)
    }

    #[test]
    fn parses_single_text_chunk() {
        let sse = b"data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n";
        let deltas = parse_sse_bytes(sse).unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].content_delta.as_deref(), Some("Hello"));
    }

    #[test]
    fn parses_multiple_text_chunks() {
        let sse = b"\
data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";
        let deltas = parse_sse_bytes(sse).unwrap();
        assert_eq!(deltas.len(), 3);
        assert_eq!(deltas[0].content_delta.as_deref(), Some("Hel"));
        assert_eq!(deltas[1].content_delta.as_deref(), Some("lo"));
        assert_eq!(deltas[2].finish_reason, Some(FinishReason::Stop));
    }

    #[test]
    fn done_marker_terminates() {
        let sse = b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\ndata: [DONE]\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"y\"}}]}\n\n";
        let deltas = parse_sse_bytes(sse).unwrap();
        assert_eq!(deltas.len(), 1, "[DONE] should stop parsing");
    }

    #[test]
    fn tool_call_chunks_assemble() {
        let sse = b"\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"bash\",\"arguments\":\"\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"command\\\":\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"ls\\\"}\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n";
        let deltas = parse_sse_bytes(sse).unwrap();
        assert_eq!(deltas.len(), 4);
        let first_tool = deltas[0].tool_call_delta.as_ref().unwrap();
        assert_eq!(first_tool.index, 0);
        assert_eq!(first_tool.id.as_deref(), Some("call_a"));
        assert_eq!(first_tool.name.as_deref(), Some("bash"));
        assert_eq!(deltas[3].finish_reason, Some(FinishReason::ToolUse));
    }

    #[test]
    fn malformed_json_yields_error() {
        let sse = b"data: {not valid json}\n\n";
        let result = parse_sse_bytes(sse);
        assert!(matches!(result, Err(ProviderError::InvalidResponse(_))));
    }

    #[test]
    fn comment_lines_are_skipped() {
        let sse = b": this is a comment\ndata: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n";
        let deltas = parse_sse_bytes(sse).unwrap();
        assert_eq!(deltas.len(), 1);
    }
}
```

- [ ] **Step 2: Run the test, confirm it fails to compile (function does not exist)**

Run: `cargo test -p hermes-providers --lib 2>&1 | tail -5`
Expected: compile error — `parse_sse_for_test` not found.

- [ ] **Step 3: Implement the SSE parser**

Add the following to `crates/hermes-providers/src/openai.rs` (replace the existing `complete` impl entirely):

```rust
use bytes::Bytes;
use futures::stream::{Stream, StreamExt};

/// Public-for-test shim. Wraps `parse_sse_chunks` with a synchronous
/// `Vec<u8>` input so unit tests can drive it without async.
#[cfg(test)]
pub(crate) fn parse_sse_for_test(input: &[u8]) -> Result<Vec<CompletionDelta>, ProviderError> {
    use futures::stream;
    let s = parse_sse_chunks(stream::once(async { Ok(Bytes::copy_from_slice(input)) }));
    let mut out = Vec::new();
    let pinned = Box::pin(s);
    let collected = futures::executor::block_on(async move {
        let mut v = Vec::new();
        let mut s = pinned;
        while let Some(item) = s.next().await {
            v.push(item?);
        }
        Ok::<_, ProviderError>(v)
    });
    collected.map(|mut v| {
        v.retain(|d| d.finish_reason.is_none() || true); // keep all
        v
    })
}

/// Parse OpenAI Server-Sent Events from a byte stream. Yields
/// `CompletionDelta` items until `[DONE]` is seen, then ends.
fn parse_sse_chunks(
    bytes: impl Stream<Item = reqwest::Result<Bytes>> + Unpin,
) -> impl Stream<Item = Result<CompletionDelta, ProviderError>> {
    // Use an async block returning a stream. Simpler: use a stateful
    // stream adapter. We use `futures::stream::unfold` to hold the buffer.
    use futures::stream::unfold;
    unfold(
        (bytes, Vec::<u8>::new()),
        move |(mut bytes, mut buffer)| async move {
            loop {
                // Look for \n\n boundary in buffer
                if let Some(pos) = buffer.windows(2).position(|w| w == b"\n\n") {
                    let event = buffer.drain(..pos + 2).collect::<Vec<u8>>();
                    let event_str = String::from_utf8_lossy(&event);
                    // Process the event: find the data: line(s)
                    let mut data_payload: Option<String> = None;
                    for line in event_str.lines() {
                        if let Some(rest) = line.strip_prefix("data: ") {
                            let payload = rest.trim();
                            if payload == "[DONE]" {
                                return Some((Ok(CompletionDelta {
                                    content_delta: None,
                                    reasoning_delta: None,
                                    tool_call_delta: None,
                                    usage: None,
                                    finish_reason: Some(FinishReason::Stop),
                                }), (bytes, buffer)).into_iter().next().map(Some));
                            }
                            data_payload = Some(payload.to_string());
                        }
                        // Skip ":..." comment lines and other SSE event types
                    }
                    if let Some(payload) = data_payload {
                        match parse_sse_data_payload(&payload) {
                            Ok(delta) => {
                                return Some((Ok(delta), (bytes, buffer)));
                            }
                            Err(e) => {
                                return Some((Err(e), (bytes, buffer)));
                            }
                        }
                    }
                    // No data payload in this event (e.g. event-only lines), keep going
                    continue;
                }
                // Need more bytes
                match bytes.next().await {
                    Some(Ok(chunk)) => {
                        buffer.extend_from_slice(&chunk);
                    }
                    Some(Err(e)) => {
                        return Some((
                            Err(ProviderError::Transport(e)),
                            (bytes, buffer),
                        ));
                    }
                    None => {
                        // Stream ended without [DONE]; end the parser
                        return None;
                    }
                }
            }
        },
    )
}
```

Wait — the `[DONE]` handling above has a bug: it yields a `Stop` delta to signal end, but the loop's `if delta.finish_reason.is_some() { break; }` will break on it. That's correct. But returning `Some((..., state).into_iter().next().map(Some))` is convoluted. Let me simplify:

Replace the entire `parse_sse_chunks` implementation with a cleaner version using `unfold` more straightforwardly:

```rust
fn parse_sse_chunks(
    mut bytes: impl Stream<Item = reqwest::Result<Bytes>> + Unpin,
) -> impl Stream<Item = Result<CompletionDelta, ProviderError>> {
    async_stream::stream! {
        let mut buffer = Vec::<u8>::new();
        loop {
            // Drain all complete events from the buffer first
            while let Some(pos) = buffer.windows(2).position(|w| w == b"\n\n") {
                let event: Vec<u8> = buffer.drain(..pos + 2).collect();
                let event_str = String::from_utf8_lossy(&event);
                let mut payload: Option<String> = None;
                for line in event_str.lines() {
                    if let Some(rest) = line.strip_prefix("data: ") {
                        payload = Some(rest.trim().to_string());
                    }
                }
                if let Some(p) = payload {
                    if p == "[DONE]" {
                        return;
                    }
                    match parse_sse_data_payload(&p) {
                        Ok(d) => yield Ok(d),
                        Err(e) => {
                            yield Err(e);
                            return;
                        }
                    }
                }
            }
            // Need more bytes
            match bytes.next().await {
                Some(Ok(chunk)) => buffer.extend_from_slice(&chunk),
                Some(Err(e)) => {
                    yield Err(ProviderError::Transport(e));
                    return;
                }
                None => return,
            }
        }
    }
}
```

This uses `async_stream::stream!`. Add to `Cargo.toml` workspace dependencies and `hermes-providers` deps:

```toml
# In workspace Cargo.toml [workspace.dependencies]:
async-stream = "0.3"

# In crates/hermes-providers/Cargo.toml [dependencies]:
async-stream.workspace = true
```

Then implement `parse_sse_data_payload`:

```rust
fn parse_sse_data_payload(payload: &str) -> Result<CompletionDelta, ProviderError> {
    #[derive(serde::Deserialize)]
    struct SseChunk {
        choices: Vec<SseChoice>,
        usage: Option<Usage>,
    }
    #[derive(serde::Deserialize)]
    struct SseChoice {
        delta: SseDelta,
        finish_reason: Option<String>,
    }
    #[derive(serde::Deserialize, Default)]
    struct SseDelta {
        content: Option<String>,
        #[serde(default)]
        reasoning_content: Option<String>,
        tool_calls: Option<Vec<SseToolCallRef>>,
    }
    #[derive(serde::Deserialize)]
    struct SseToolCallRef {
        index: usize,
        id: Option<String>,
        function: SseFunction,
    }
    #[derive(serde::Deserialize, Default)]
    struct SseFunction {
        name: Option<String>,
        arguments: Option<String>,
    }

    let chunk: SseChunk = serde_json::from_str(payload)
        .map_err(|e| ProviderError::InvalidResponse(format!("sse json: {e}")))?;

    let choice = chunk.choices.into_iter().next()
        .ok_or_else(|| ProviderError::InvalidResponse("sse: no choices".into()))?;

    let tool_call_delta = choice.delta.tool_calls.and_then(|calls| {
        calls.into_iter().next().map(|c| ToolCallDelta {
            index: c.index,
            id: c.id,
            name: c.function.name,
            arguments_delta: c.function.arguments,
        })
    });

    Ok(CompletionDelta {
        content_delta: choice.delta.content,
        reasoning_delta: choice.delta.reasoning_content,
        tool_call_delta,
        usage: chunk.usage,
        finish_reason: choice.finish_reason.as_deref().map(FinishReason::from_provider_str),
    })
}
```

Note: per spec §3.3, multiple `tool_calls` in one delta are technically possible (parallel). For minimum viable, we take only the first in each chunk. The accumulator handles interleaving by `index` correctly — but we lose tool_calls after the first in the same chunk. **This is a known limitation** of the v1 parser. Document it in a code comment and revisit if real users hit it. Most streaming responses emit at most one tool_call per chunk anyway.

Then implement the actual `stream()` method (replacing the old `complete()`):

```rust
#[async_trait]
impl Provider for OpenAiProvider {
    fn name(&self) -> &str { "openai" }
    fn model(&self) -> &str { &self.model }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<CompletionStream, ProviderError> {
        let body = build_request_body(&self.model, messages, tools)?;
        let url = format!("{}/chat/completions", self.base_url);

        // Pre-flight: send request, race against cancel
        let resp = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Err(ProviderError::Cancelled);
            }
            r = self.client.post(&url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .send() => r.map_err(ProviderError::Transport)?,
        };

        // Pre-flight: status check (before consuming body)
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

        // Hand the byte stream to the SSE parser.
        Ok(Box::pin(parse_sse_chunks(resp.bytes_stream())))
    }
}
```

And `build_request_body`:

```rust
#[derive(serde::Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<OaiMessage<'a>>,
    tools: Vec<OaiTool<'a>>,
    tool_choice: Option<&'static str>,
    stream: bool,
}

fn build_request_body<'a>(
    model: &'a str,
    messages: &'a [Message],
    tools: &'a [ToolSchema],
) -> Result<ChatRequest<'a>, ProviderError> {
    let oai_msgs: Vec<OaiMessage> = messages.iter().map(|m| {
        // ... copy the existing mapping logic from old complete() ...
    }).collect();

    let oai_tools: Vec<OaiTool> = tools.iter().map(|t| OaiTool { /* ... */ }).collect();

    Ok(ChatRequest {
        model,
        messages: oai_msgs,
        tools: oai_tools,
        tool_choice: if tools.is_empty() { None } else { Some("auto") },
        stream: true,
    })
}
```

(Port the `OaiMessage` / `OaiTool` / etc. structs verbatim from the existing `complete()` impl — they're identical apart from `stream: true` and `tool_choice: Option`.)

- [ ] **Step 4: Run the SSE parser tests**

Run: `cargo test -p hermes-providers --lib`
Expected: 6 SSE parser tests pass.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/hermes-providers/Cargo.toml crates/hermes-providers/src/openai.rs
git commit -m "feat(providers): OpenAiProvider::stream with SSE parser"
```

---

## Task 8: OpenAI SSE integration test with `tokio::net::TcpListener`

**Files:**
- Create: `crates/hermes-providers/tests/openai_stream.rs`

Per CLAUDE.md, httpmock 0.7 doesn't expose captured bodies — we drive a raw TCP server.

- [ ] **Step 1: Write the test**

```rust
//! Integration test for OpenAiProvider::stream using a raw TcpListener
//! to serve canned SSE bytes (httpmock doesn't expose captured bodies
//! per CLAUDE.md).

use std::time::Duration;
use futures::StreamExt;
use hermes_core::provider::{CompletionDelta, FinishReason, Provider};
use hermes_providers::OpenAiProvider;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const SAMPLE_BODY: &[u8] = b"\
data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";

#[tokio::test]
async fn stream_parses_sse_chunks() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        // Read the request (don't bother parsing — just consume it)
        let mut buf = [0u8; 4096];
        let _ = tokio::time::timeout(Duration::from_millis(500), socket.read(&mut buf)).await;
        // Respond with SSE
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n"
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.write_all(SAMPLE_BODY).await.unwrap();
        socket.flush().await.unwrap();
        // Keep socket open briefly so the client can drain
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let provider = OpenAiProvider::new("test-key", "gpt-test")
        .with_base_url(format!("http://{addr}"));
    let cancel = CancellationToken::new();
    let messages = vec![];
    let tools = vec![];

    let mut stream = provider.stream(&messages, &tools, cancel).await.unwrap();
    let mut deltas = Vec::new();
    while let Some(item) = stream.next().await {
        deltas.push(item.unwrap());
    }
    server.await.unwrap();

    assert_eq!(deltas.len(), 3);
    assert_eq!(deltas[0].content_delta.as_deref(), Some("Hel"));
    assert_eq!(deltas[1].content_delta.as_deref(), Some("lo"));
    assert_eq!(deltas[2].finish_reason, Some(FinishReason::Stop));
}

use tokio::io::AsyncReadExt;
```

- [ ] **Step 2: Run, confirm it passes**

Run: `cargo test -p hermes-providers --test openai_stream -- --nocapture`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-providers/tests/openai_stream.rs
git commit -m "test(providers): integration test for OpenAI SSE stream"
```

---

## Task 9: Add new `LoopEvent` variants

**Files:**
- Modify: `crates/hermes-loop/src/agent.rs:93-107`

Add `ContentDelta(String)`, `ReasoningDelta(String)`, `ToolCallPartial(ToolCallDelta)`.

- [ ] **Step 1: Edit the enum**

```rust
use hermes_core::provider::ToolCallDelta;

#[derive(Debug, Clone)]
pub enum LoopEvent {
    Thinking,
    /// One text token from a streaming assistant message.
    ContentDelta(String),
    /// One reasoning token (o1, extended thinking).
    ReasoningDelta(String),
    /// A delta for a streaming tool call. Silent-accumulated by the loop;
    /// `ToolCallStarted` fires only when the call is complete.
    ToolCallPartial(ToolCallDelta),
    AssistantMessage(Message),
    ToolCallStarted {
        call: ToolCall,
        iteration: u32,
    },
    ToolCallFinished {
        call: ToolCall,
        result: Result<ToolOutput, hermes_core::error::ToolError>,
    },
    LengthLimit,
    IterationsExhausted,
    Cancelled,
}
```

- [ ] **Step 2: Verify it compiles (expect errors in the loop, fix in next task)**

Run: `cargo build -p hermes-loop 2>&1 | grep -E "^error" | head -5`
Expected: errors because `agent.rs`'s existing code uses the old `LoopEvent` patterns but doesn't construct any of the new variants. Since `on_event: impl FnMut` is the consumer, the only way the new variants fail compilation is if some other code path references `LoopEvent::*` exhaustively. They don't — `on_event` is a closure. So this should compile cleanly.

Run: `cargo build -p hermes-loop 2>&1 | tail -3`
Expected: clean (or only warnings).

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-loop/src/agent.rs
git commit -m "feat(loop): add ContentDelta/ReasoningDelta/ToolCallPartial events"
```

---

## Task 10: Rewrite `AgentLoop::run` to use stream

**Files:**
- Modify: `crates/hermes-loop/src/agent.rs:149-180` (the section that calls `provider.complete`)

Replace the one-shot `complete()` call with a stream-driven loop that emits deltas and assembles the final `Completion`.

- [ ] **Step 1: Replace the LLM call section**

Find the block:
```rust
            // ── 3. Call the LLM ────────────────────────────────────
            on_event(LoopEvent::Thinking);
            let completion = self
                .provider
                .complete(&messages, &tools, cancel.clone())
                .await?;
            metrics.iterations += 1;
            metrics.input_tokens += completion.usage.input_tokens;
            metrics.output_tokens += completion.usage.output_tokens;
```

Replace with:
```rust
            // ── 3. Call the LLM (streaming) ────────────────────────
            on_event(LoopEvent::Thinking);
            let mut stream = self
                .provider
                .stream(&messages, &tools, cancel.clone())
                .await?;
            let mut acc = hermes_core::provider::StreamAccumulator::new();
            let completion = loop {
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
                                if delta.finish_reason.is_some() {
                                    break acc.finalize();
                                }
                            }
                            Some(Err(e)) => return Err(LoopError::Provider(e)),
                            None => break acc.finalize(),
                        }
                    }
                }
            };
            metrics.iterations += 1;
            metrics.input_tokens += completion.usage.input_tokens;
            metrics.output_tokens += completion.usage.output_tokens;
```

Add `use futures::StreamExt;` at the top of the file (needed for `.next()` on the stream).

- [ ] **Step 2: Verify it compiles (expect ScriptedProvider test breakage, fix in Task 11)**

Run: `cargo build -p hermes-loop 2>&1 | tail -3`
Expected: clean.

Run: `cargo test -p hermes-loop 2>&1 | grep -E "^(error|test result)" | head -10`
Expected: ScriptedProvider tests fail because they implement `complete` not `stream`. Fix in next task.

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-loop/src/agent.rs
git commit -m "feat(loop): AgentLoop::run uses provider.stream, emits deltas, handles cancel"
```

---

## Task 11: Update `ScriptedProvider` in test files to implement `stream`

**Files:**
- Modify: `crates/hermes-loop/tests/arg_validation.rs:21-79` (`ScriptedProvider` struct + impl)
- Modify: `crates/hermes-loop/tests/tool_dispatch.rs:28-83` (same)

Both files have a near-identical `ScriptedProvider`. Both must change in lockstep.

- [ ] **Step 1: Replace `ScriptedProvider` in `arg_validation.rs`**

Read the existing struct (lines 21–79), then replace it with:

```rust
use futures::stream;
use hermes_core::provider::{CompletionDelta, CompletionStream, FinishReason, Provider};

struct ScriptedProvider {
    // The script is a list of (Vec<CompletionDelta>) — one inner vec per
    // call to `stream()`. The default `complete()` impl drives each
    // scripted stream through `accumulate_stream` to produce a Completion.
    script: std::sync::Mutex<Vec<Vec<CompletionDelta>>>,
    call_count: std::sync::atomic::AtomicUsize,
}

impl ScriptedProvider {
    fn new(script: Vec<Completion>) -> Self {
        // Convert each scripted Completion into the equivalent sequence of
        // deltas the loop would see if the provider were streaming.
        let script: Vec<Vec<CompletionDelta>> = script.into_iter().map(completion_to_deltas).collect();
        Self {
            script: std::sync::Mutex::new(script),
            call_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

fn completion_to_deltas(c: Completion) -> Vec<CompletionDelta> {
    // Mirror the structure StreamAccumulator::add expects.
    let mut deltas = Vec::new();
    if let Content::Text(text) = &c.message.content {
        if !text.is_empty() {
            deltas.push(CompletionDelta {
                content_delta: Some(text.clone()),
                reasoning_delta: c.message.reasoning.clone(),
                tool_call_delta: None,
                usage: Some(c.usage),
                finish_reason: None,
            });
        }
    }
    if let Some(calls) = &c.message.tool_calls {
        for (i, tc) in calls.iter().enumerate() {
            deltas.push(CompletionDelta {
                content_delta: None,
                reasoning_delta: None,
                tool_call_delta: Some(ToolCallDelta {
                    index: i,
                    id: Some(tc.id.clone()),
                    name: Some(tc.name.clone()),
                    arguments_delta: Some(tc.arguments.to_string()),
                }),
                usage: None,
                finish_reason: None,
            });
        }
    }
    // Final delta carries the finish_reason (and usage if not already)
    deltas.push(CompletionDelta {
        content_delta: None,
        reasoning_delta: None,
        tool_call_delta: None,
        usage: Some(c.usage),
        finish_reason: Some(c.finish_reason),
    });
    deltas
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn name(&self) -> &str { "scripted" }
    fn model(&self) -> &str { "scripted-v0" }

    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _cancel: CancellationToken,
    ) -> Result<CompletionStream, ProviderError> {
        let mut script = self.script.lock().unwrap();
        if script.is_empty() {
            panic!("ScriptedProvider: script exhausted — the loop called stream() more times than scripted");
        }
        let deltas = script.remove(0);
        self.call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Box::pin(stream::iter(deltas.into_iter().map(Ok))))
    }
}
```

Add `use hermes_core::provider::ToolCallDelta;` to the test file imports.

- [ ] **Step 2: Repeat the same replacement in `tool_dispatch.rs`**

The struct and `Provider` impl are duplicated. Apply the same replacement.

- [ ] **Step 3: Run the loop tests**

Run: `cargo test -p hermes-loop`
Expected: all tests pass (echo_loop, arg_validation, tool_dispatch).

- [ ] **Step 4: Commit**

```bash
git add crates/hermes-loop/tests/arg_validation.rs crates/hermes-loop/tests/tool_dispatch.rs
git commit -m "test(loop): update ScriptedProvider to implement stream"
```

---

## Task 12: Update CLI `on_event` for new variants + `CancelledWith` history push

**Files:**
- Modify: `crates/hermes-cli/src/main.rs` (the `on_event` closure + the `Err` match arm)

- [ ] **Step 1: Add `ContentDelta` / `ReasoningDelta` / `ToolCallPartial` arms**

Find the existing `|event| match event { ... }` closure in `run_repl` and add three new arms. Also add `use hermes_loop::LoopEvent;` if not already (likely already imported).

```rust
                LoopEvent::ContentDelta(s) => {
                    eprint!("{s}");
                    let _ = stdout.flush();
                }
                LoopEvent::ReasoningDelta(s) => {
                    eprint!("\x1b[2m{s}\x1b[0m");
                    let _ = stdout.flush();
                }
                LoopEvent::ToolCallPartial(_) => {
                    // Silent — ToolCallStarted fires when complete.
                }
                // ... existing arms unchanged ...
```

- [ ] **Step 2: Handle `LoopError::CancelledWith` in the post-run `match`**

Find the `Err(e) => { eprintln!("error: {e}"); history.pop(); ... }` block and add a new arm before it:

```rust
            Err(LoopError::CancelledWith(partial)) => {
                let chars = match &partial.content {
                    Content::Text(s) => s.chars().count(),
                    Content::Parts(_) => 0,
                };
                let calls = partial.tool_calls.as_ref().map(|c| c.len()).unwrap_or(0);
                eprintln!(
                    "  [cancelled mid-stream: {chars} chars streamed, {calls} tool call kept]"
                );
                if chars > 0 || calls > 0 {
                    history.push(partial);
                } else {
                    history.pop();
                }
                eprintln!();
            }
            Err(LoopError::Cancelled) => {
                eprintln!("[cancelled]");
                history.pop();
                eprintln!();
            }
            Err(e) => {
                eprintln!("error: {e}");
                history.pop();
                eprintln!();
            }
```

(Add `use hermes_core::error::LoopError;` if not already imported.)

- [ ] **Step 3: Verify it compiles**

Run: `cargo build -p hermes-cli`
Expected: clean.

- [ ] **Step 4: Smoke test with echo provider**

Run: `printf 'hello\n/exit\n' | cargo run -p hermes-cli --quiet -- --provider echo`
Expected: 
- `hermes v0.1.0 — type a message, ...`
- `… echo: hello`
- `[iterations=1 ...]`

- [ ] **Step 5: Commit**

```bash
git add crates/hermes-cli/src/main.rs
git commit -m "feat(cli): render streaming deltas, handle CancelledWith"
```

---

## Task 13: Full verification

- [ ] **Step 1: Run full workspace test suite**

Run: `cargo test --workspace`
Expected: all tests green (hermes-core, hermes-providers, hermes-loop, hermes-tools, hermes-cli, hermes-runtime, hermes-loop/tests/).

- [ ] **Step 2: Run clippy with `-D warnings`**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 3: CLI smoke with echo provider**

Run: `printf 'hello\n/exit\n' | cargo run -p hermes-cli --quiet -- --provider echo`
Expected: see the smoke output from Task 12 step 4.

- [ ] **Step 4: CLI smoke with a real OpenAI streaming response** (only if `OPENAI_API_KEY` is set)

Run: `printf 'say hi in 5 words\n/exit\n' | cargo run -p hermes-cli --quiet -- --provider openai`
Expected: tokens appear on stderr one by one (or in small chunks); final text appears; metrics line.

- [ ] **Step 5: Update CLAUDE.md "Known Issues" section** (historical task; CLAUDE.md has since been updated again after the runtime/registry simplification)

Edit the "Still open (before phase 5)" list — both items are now resolved:
- Permission model is still coarse — `BashTool` checks `subprocess`, but runtime currently always enables it.
- Unknown `finish_reason` maps to `FinishReason::Error`, but provider diagnostics are still coarse.
- `Content::Parts` only sends the first text part to OpenAI-compatible providers; real multimodal mapping is still missing.
- `BashTool` does not kill concurrent children — still open.
- **Resolved:** "No streaming yet (Phase 5) — CLI blocks until full completion." → remove.

- [ ] **Step 6: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: mark phase 5 streaming as resolved"
```

---

## Self-Review Notes (for the planner)

- **Spec coverage:**
  - §3.2 (data flow, no tools) → Task 10
  - §3.3 (data flow, tool calls) → Task 7 (provider emits per-index deltas) + Task 2 (accumulator) + Task 10 (loop)
  - §4.1 (Provider trait) → Task 4
  - §4.2 (ToolCallDelta) → Task 1
  - §4.3 (LoopEvent) → Task 9
  - §4.4 (LoopError::CancelledWith) → Task 5
  - §5.1 (OpenAI SSE) → Task 7
  - §5.2 (EchoProvider) → Task 6
  - §6 (all error paths) → Task 10 (cancel/network) + Task 7 (HTTP/SSE errors)
  - §7 (StreamAccumulator + AgentLoop rewrite) → Tasks 2, 3, 10
  - §8 (CLI rendering) → Task 12
  - §9 (testing) → Tasks 2, 3, 7, 8, 11
  - §10 (migration) → no separate task — covered incrementally as each file is updated
  - §11 (out of scope) → no task, not in scope

- **Placeholder scan:** "/* existing logic */" in Task 7's `build_request_body` is intentional — the engineer should copy the `OaiMessage`/`OaiTool` mapping from the existing `complete()` impl. Wording clarifies this in the step body.

- **Type consistency:** `StreamAccumulator::add`, `into_partial_message`, `is_empty`, `finalize` all consistent across Tasks 2, 7, 10, 12. `LoopError::CancelledWith(Message)` referenced identically in Tasks 5, 10, 12. `ToolCallDelta` fields consistent across Tasks 1, 2, 7.

- **Known limitation called out:** Task 7's `parse_sse_data_payload` only takes the first `tool_calls` entry per chunk. Most streaming responses emit at most one per chunk; the code comment flags this for future work.
