# Split Large Files — StreamAccumulator Extraction + SSE Boundary Tests

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract `StreamAccumulator` from `hermes-core/src/provider.rs` into its own `accumulator.rs` file, and add 4 boundary tests to each provider's existing SSE parser test block.

**Architecture:** Mechanical code move + additive test coverage. No behavior changes. Public API paths preserved via `pub use` re-export. The two provider files (`openai.rs`, `anthropic.rs`) are NOT split into directories in this round — that's deferred to §8 of the spec.

**Tech Stack:** Rust 1.x, `tokio`, `reqwest`, `bytes`, `futures`, `async_stream`, `serde_json`. TDD per `CLAUDE.md` — strict RED → GREEN → REFACTOR.

**Reference spec:** `docs/superpowers/specs/2026-06-05-split-large-files-design.md`

---

## File Structure

| File | Change |
|---|---|
| `crates/hermes-core/src/accumulator.rs` | **CREATE** — moved `StreamAccumulator`, `accumulate_stream`, and the 8 existing accumulator tests (verbatim) |
| `crates/hermes-core/src/lib.rs` | **MODIFY** — add `pub mod accumulator;` (one new line) |
| `crates/hermes-core/src/provider.rs` | **MODIFY** — remove the moved code, add `pub use crate::accumulator::{accumulate_stream, StreamAccumulator};` at the bottom, update the `Provider::complete` default impl to use the new path |
| `crates/hermes-providers/src/openai.rs` | **MODIFY** — add 4 boundary tests to the existing `mod tests` block |
| `crates/hermes-providers/src/anthropic.rs` | **MODIFY** — add 4 boundary tests to the existing `mod tests` block |

No new files, no new modules, no new public API surface.

---

## Task 1: Add 4 SSE Boundary Tests to OpenAI's Test Block

**Files:**
- Modify: `crates/hermes-providers/src/openai.rs` (append 4 tests to the existing `#[cfg(test)] mod tests` block at the end of the file)

The OpenAI `mod tests` block already has 8 tests and a `parse_sse_bytes` helper that wraps `parse_sse_chunks` with a single byte chunk. We add 4 new tests that exercise boundary conditions: chunked bytes, transport errors, the `[DONE]` sentinel preserving prior deltas, and partial UTF-8 input.

- [ ] **Step 1: Add the `chunks_split_across_frames_assemble_correctly` test**

Append this test inside the existing `mod tests` block in `crates/hermes-providers/src/openai.rs` (before the closing `}` of the module):

```rust
    #[test]
    fn chunks_split_across_frames_assemble_correctly() {
        // A single SSE event must survive being split across multiple
        // byte chunks. The "\n\n" frame boundary can land in the middle
        // of a chunk; the parser must still produce one delta.
        use futures::stream;
        let chunks: Vec<&[u8]> = vec![
            b"data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}]}\n",
            b"\n",
            b"data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n",
        ];
        let byte_stream = stream::iter(
            chunks
                .iter()
                .map(|c| Ok::<_, reqwest::Error>(Bytes::copy_from_slice(c)))
                .collect::<Vec<_>>(),
        );
        let s = parse_sse_chunks(byte_stream);
        let deltas: Vec<CompletionDelta> = futures::executor::block_on(async move {
            let mut v = Vec::new();
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                v.push(item.unwrap());
            }
            v
        });
        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].content_delta.as_deref(), Some("Hel"));
        assert_eq!(deltas[1].content_delta.as_deref(), Some("lo"));
    }
```

- [ ] **Step 2: Add the `transport_error_becomes_provider_error_transport` test**

Append this test inside `mod tests`:

```rust
    #[test]
    fn transport_error_becomes_provider_error_transport() {
        // A byte-stream that yields Err(reqwest::Error) must propagate
        // as ProviderError::Transport. Construct the error from a
        // std::io::Error (reqwest::Error has From<io::Error>).
        use futures::stream;
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "test");
        let reqwest_err: reqwest::Error = reqwest::Error::from(io_err);
        let byte_stream = stream::iter(vec![Err::<Bytes, _>(reqwest_err)]);
        let s = parse_sse_chunks(byte_stream);
        let result: ProviderError = futures::executor::block_on(async move {
            let mut err = None;
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                if let Err(e) = item {
                    err = Some(e);
                    break;
                }
            }
            err.expect("stream must yield an error")
        });
        assert!(matches!(result, ProviderError::Transport(_)));
    }
```

- [ ] **Step 3: Add the `done_sentinel_preserves_prior_deltas` test**

Append this test inside `mod tests`:

```rust
    #[test]
    fn done_sentinel_preserves_prior_deltas() {
        // Two valid events, then DONE, then a third valid event. The
        // third must NOT be parsed — the stream terminates at the
        // sentinel. (Note: the existing `done_marker_terminates` test
        // covers the 1-event case; this one specifically asserts that
        // both prior deltas are kept.)
        let sse = b"\
data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"y\"}}]}\n\n\
data: [DONE]\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"z\"}}]}\n\n";
        let deltas = parse_sse_bytes(sse).unwrap();
        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].content_delta.as_deref(), Some("x"));
        assert_eq!(deltas[1].content_delta.as_deref(), Some("y"));
    }
```

- [ ] **Step 4: Add the `partial_utf8_in_a_chunk_does_not_panic` test**

Append this test inside `mod tests`:

```rust
    #[test]
    fn partial_utf8_in_a_chunk_does_not_panic() {
        // Bytes containing invalid UTF-8 inside a data: line. The
        // parser uses String::from_utf8_lossy, so it must not panic.
        // The exact outcome is unspecified (could be Err
        // InvalidResponse for the malformed JSON, or skip the event);
        // we only assert that the call returns rather than panicking.
        let sse = b"data: \xFF\xFE\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n";
        let result = parse_sse_bytes(sse);
        // Smoke: does not panic; returns either Ok or Err cleanly.
        let _ = result;
    }
```

- [ ] **Step 5: Run the tests and verify all 4 new tests pass**

Run: `cargo test -p hermes-providers openai::tests::`
Expected: All existing 8 OpenAI tests still pass + 4 new tests pass. 12 total.

If any test fails, that means the existing parser has a real boundary bug. Fix the parser minimally, document the fix in a code comment, and proceed.

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-providers/src/openai.rs
git commit -m "test(providers): add 4 SSE boundary tests to OpenAI parser"
```

---

## Task 2: Add 4 SSE Boundary Tests to Anthropic's Test Block

**Files:**
- Modify: `crates/hermes-providers/src/anthropic.rs` (append 4 tests to the existing `#[cfg(test)] mod tests` block at the end of the file)

Anthropic's `mod tests` block has 9 existing tests that use `parse_sse_chunks` directly (no helper function). We add 4 new tests mirroring Task 1's coverage but for the Anthropic SSE event schema. Since Anthropic has no `[DONE]` sentinel, we replace that test with a `message_stop_event_terminates_cleanly` test.

- [ ] **Step 1: Add the `chunks_split_across_frames_assemble_correctly` test**

Append this test inside the existing `mod tests` block in `crates/hermes-providers/src/anthropic.rs`:

```rust
    #[test]
    fn chunks_split_across_frames_assemble_correctly() {
        use futures::stream;
        let chunks: Vec<&[u8]> = vec![
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n",
            b"\n",
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n",
        ];
        let byte_stream = stream::iter(
            chunks
                .iter()
                .map(|c| Ok::<_, reqwest::Error>(Bytes::copy_from_slice(c)))
                .collect::<Vec<_>>(),
        );
        let s = parse_sse_chunks(byte_stream);
        let deltas: Vec<CompletionDelta> = futures::executor::block_on(async move {
            let mut v = Vec::new();
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                v.push(item.unwrap());
            }
            v
        });
        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].content_delta.as_deref(), Some("Hel"));
        assert_eq!(deltas[1].content_delta.as_deref(), Some("lo"));
    }
```

- [ ] **Step 2: Add the `transport_error_becomes_provider_error_transport` test**

Append this test inside `mod tests`:

```rust
    #[test]
    fn transport_error_becomes_provider_error_transport() {
        use futures::stream;
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "test");
        let reqwest_err: reqwest::Error = reqwest::Error::from(io_err);
        let byte_stream = stream::iter(vec![Err::<Bytes, _>(reqwest_err)]);
        let s = parse_sse_chunks(byte_stream);
        let result: ProviderError = futures::executor::block_on(async move {
            let mut err = None;
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                if let Err(e) = item {
                    err = Some(e);
                    break;
                }
            }
            err.expect("stream must yield an error")
        });
        assert!(matches!(result, ProviderError::Transport(_)));
    }
```

- [ ] **Step 3: Add the `message_stop_event_terminates_cleanly` test**

Append this test inside `mod tests`:

```rust
    #[test]
    fn message_stop_event_terminates_cleanly() {
        // Anthropic uses explicit event types: message_delta carries
        // stop_reason + usage, then message_stop is the actual end
        // marker. The parser must yield the message_delta delta and
        // then return cleanly when message_stop arrives (no extra
        // Ok(None) leak).
        let input = b"\
event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":0}}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n\
event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n\
";
        let s = parse_sse_chunks(stream::iter(vec![Ok::<_, reqwest::Error>(
            Bytes::copy_from_slice(input),
        )]));

        let deltas = futures::executor::block_on(async move {
            let mut out = Vec::new();
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                out.push(item.unwrap());
            }
            out
        });

        // Expect: message_start yields usage delta, content_block_delta
        // yields text delta, message_delta yields finish_reason + usage.
        // message_stop is silently consumed and yields nothing extra.
        assert_eq!(deltas.len(), 3);
        assert_eq!(deltas[0].usage.unwrap().input_tokens, 2);
        assert_eq!(deltas[1].content_delta.as_deref(), Some("Hi"));
        assert_eq!(deltas[2].finish_reason, Some(FinishReason::Stop));
        assert_eq!(deltas[2].usage.unwrap().output_tokens, 4);
    }
```

- [ ] **Step 4: Add the `partial_utf8_in_a_chunk_does_not_panic` test**

Append this test inside `mod tests`:

```rust
    #[test]
    fn partial_utf8_in_a_chunk_does_not_panic() {
        use futures::stream;
        let s = parse_sse_chunks(stream::iter(vec![Ok::<_, reqwest::Error>(
            Bytes::copy_from_slice(b"data: \xFF\xFE\n\n"),
        )]));
        let result: Result<Vec<CompletionDelta>, _> = futures::executor::block_on(async move {
            let mut v = Vec::new();
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                v.push(item?);
            }
            Ok(v)
        });
        // Smoke: does not panic; returns Ok(empty) or Err cleanly.
        let _ = result;
    }
```

- [ ] **Step 5: Run the tests and verify all 4 new tests pass**

Run: `cargo test -p hermes-providers anthropic::tests::`
Expected: All existing 9 Anthropic tests still pass + 4 new tests pass. 13 total.

If any test fails, the existing parser has a real boundary bug. Fix the parser minimally, document the fix, and proceed.

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-providers/src/anthropic.rs
git commit -m "test(providers): add 4 SSE boundary tests to Anthropic parser"
```

---

## Task 3: Extract `StreamAccumulator` to `accumulator.rs`

**Files:**
- Create: `crates/hermes-core/src/accumulator.rs`
- Modify: `crates/hermes-core/src/lib.rs` (add `pub mod accumulator;`)
- Modify: `crates/hermes-core/src/provider.rs` (remove moved code, add re-export, fix `Provider::complete` default impl path)

This is a mechanical code move. The struct definition, all its methods, the `accumulate_stream` function, and the 8 existing tests in `provider.rs::mod tests` move verbatim to the new file. `use` paths inside the moved code adjust from same-file references to `crate::provider::...` references.

- [ ] **Step 1: Create `crates/hermes-core/src/accumulator.rs` with the moved code**

Create the file with this exact content:

```rust
//! `StreamAccumulator` — turns a stream of `CompletionDelta`s into a
//! final `Completion` (or a partial `Message` for cancellation).
//!
//! Lives in its own file because it is a 130-line cohesive concept that
//! is not part of the `Provider` trait surface. `Provider::complete`'s
//! default impl re-exports `accumulate_stream` from here.

use std::collections::BTreeMap;

use futures::StreamExt;

use crate::error::ProviderError;
use crate::message::{Content, Message, Role, ToolCall};
use crate::provider::{Completion, CompletionDelta, CompletionStream, FinishReason};
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
        self.content.is_empty() && self.reasoning.is_empty() && self.tool_calls.is_empty()
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

/// Drive a `CompletionStream` to completion and return the final `Completion`.
///
/// This is a public helper used by the default `Provider::complete` impl.
/// It does NOT emit per-delta events — for that, use `AgentLoop::run` which
/// has its own private drive loop.
pub async fn accumulate_stream(mut stream: CompletionStream) -> Result<Completion, ProviderError> {
    let mut acc = StreamAccumulator::new();
    while let Some(item) = stream.next().await {
        let delta = item?;
        acc.add(&delta);
    }
    Ok(acc.finalize())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
                usage: Some(Usage {
                    input_tokens: 1,
                    output_tokens: 2,
                    cached_input_tokens: 0,
                }),
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
        // Fixed bug: plan had `Some(None)` which accumulates "None" into reasoning
        acc.add(&delta(Some("world"), None));
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

- [ ] **Step 2: Add `pub mod accumulator;` to `crates/hermes-core/src/lib.rs`**

Modify `crates/hermes-core/src/lib.rs`. Add the line `pub mod accumulator;` to the `pub mod` list, positioned between `pub mod provider;` and `pub mod registry;` (alphabetical-ish, or just after `provider` since the module is the partner of `provider`):

Change the file from:
```rust
pub mod error;
pub mod message;
pub mod provider;
pub mod registry;
pub mod tool;
pub mod usage;
```

To:
```rust
pub mod accumulator;
pub mod error;
pub mod message;
pub mod provider;
pub mod registry;
pub mod tool;
pub mod usage;
```

- [ ] **Step 3: Remove the moved code from `provider.rs` and add the re-export**

In `crates/hermes-core/src/provider.rs`:

1. Delete lines 113-265 (the section comment `// --- StreamAccumulator ---` through the end of the `accumulate_stream` function).
2. Delete lines 271-468 (the entire `#[cfg(test)] mod tests { ... }` block at the bottom).
3. After the deleted code, append the re-export at the very end of the file:

```rust
// Re-exported to preserve the `hermes_core::provider::StreamAccumulator`
// import path used by `hermes-loop`. The implementation lives in
// `crate::accumulator`; `provider.rs` keeps the trait + delta types only.
pub use crate::accumulator::{accumulate_stream, StreamAccumulator};
```

- [ ] **Step 4: Update the `Provider::complete` default impl to use the new path**

In `crates/hermes-core/src/provider.rs`, the `Provider` trait's default `complete` impl at lines 32-40 currently calls `accumulate_stream` (same-file reference). Change it to call `crate::accumulator::accumulate_stream`:

Change from:
```rust
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<Completion, ProviderError> {
        let stream = self.stream(messages, tools, cancel).await?;
        accumulate_stream(stream).await
    }
```

To:
```rust
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<Completion, ProviderError> {
        let stream = self.stream(messages, tools, cancel).await?;
        crate::accumulator::accumulate_stream(stream).await
    }
```

- [ ] **Step 5: Verify the build compiles and all tests pass**

Run: `cargo build`
Expected: clean build, no warnings.

Run: `cargo test -p hermes-core`
Expected: all 8 tests in the new `accumulator.rs::tests` block pass. (The `provider.rs` file has no tests anymore — the re-export at the bottom of the file doesn't have any tests of its own.)

Run: `cargo test`
Expected: all 12 OpenAI SSE tests, all 13 Anthropic SSE tests, all 8 StreamAccumulator tests, plus every other test in the workspace passes.

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean, no warnings.

If `clippy` flags anything, fix the warning. Common possibilities:
- Unused import in `accumulator.rs` if the new file doesn't need something the old file had.
- A `use super::*;` test import that's now redundant.

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-core/src/accumulator.rs crates/hermes-core/src/lib.rs crates/hermes-core/src/provider.rs
git commit -m "refactor(core): extract StreamAccumulator to its own file"
```

---

## Self-Review

**Spec coverage check** — every spec requirement maps to a task:

- §1 Goal: "StreamAccumulator lives in its own file" → Task 3, Steps 1-4
- §1 Goal: "Existing SSE parsers have boundary tests" → Task 1 (OpenAI), Task 2 (Anthropic)
- §1 Goal: "Public API paths stay byte-identical" → Task 3, Step 3 (re-export at bottom of `provider.rs`)
- §1 Goal: "All existing tests keep passing" → Task 3, Step 5 (verify)
- §1 Goal: "New tests cover gaps in the SSE parsers" → Tasks 1 and 2, all 4 tests each
- §4.2 "Re-export at bottom of `provider.rs`" → Task 3, Step 3
- §4.2 "agent.rs not touched" → Task 3 does not edit `agent.rs`
- §4.2 "`hermes-core/src/lib.rs` gains `pub mod accumulator;`" → Task 3, Step 2
- §4.3 Eight boundary tests across both providers → Task 1 (4 OpenAI), Task 2 (4 Anthropic)
- §5 TDD workflow (RED → GREEN → REFACTOR) → Task 1+2 first (RED: add tests, they pass — current parsers are correct; GREEN: nothing more to do because parsers are correct), Task 3 is a mechanical move with verify step
- §7 Test coverage summary table (8 new tests by name) → reproduced in Tasks 1 and 2
- §8 Out-of-scope parking lot → not implemented, no task touches SSE helper, provider splits, agent.rs decomp, etc.

**No placeholders** — every code block is complete. No "TBD", "TODO", "fill in details", "similar to Task N", or vague steps.

**Type consistency** — `StreamAccumulator` and `accumulate_stream` are spelled identically across Tasks 1, 2, and 3. The re-export names in Step 3 match the existing import path `hermes_core::provider::StreamAccumulator` used by `agent.rs:186`. The 4 OpenAI test names and 4 Anthropic test names match the spec's §7 table exactly.

**Plan is self-contained and complete.** Total: 3 tasks, ~18 numbered steps across them, all bite-sized, with exact code, exact commands, and expected output for every test/build run.
