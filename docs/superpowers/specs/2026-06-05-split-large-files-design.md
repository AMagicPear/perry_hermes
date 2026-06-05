# Split Large Files — Extract `StreamAccumulator`, Add SSE Parser Boundary Tests

**Date:** 2026-06-05
**Status:** Designed. Pending user review before writing the implementation plan.
**Scope:** `hermes-core` (one extraction) + `hermes-providers` (boundary tests only).

## 1. Goals

The codebase has accumulated 800-line and 540-line files. After reviewing internal structure, two real refactors are worth doing now; the rest are not.

After this refactor:

- **`StreamAccumulator` lives in its own file** (`hermes-core/src/accumulator.rs`). It is a 130-line cohesive concept (stream-delta aggregation, partial-message construction for cancel) that is not part of the `Provider` trait surface — putting it in `provider.rs` made that file misleadingly long.
- **Existing SSE parsers in OpenAI and Anthropic have boundary tests** for the cases that aren't covered today: bytes split across multiple chunks, transport errors, the `[DONE]`-then-extra-event edge case. This is a low-cost test-coverage upgrade on code that's already working, no behavior change.
- Public API paths stay byte-identical for everything that's currently public.
- All existing tests keep passing; new tests cover gaps in the SSE parsers.

## 2. Non-Goals

Explicitly out of scope for this round. Each is a separate brainstorm.

- **Shared `sse_frame_stream` helper across OpenAI and Anthropic.** The two providers' SSE parsers look similar on the surface (both buffer bytes, split on `\n\n`, extract `data:` lines) but differ in two material ways:
  - **OpenAI** iterates all `data:` lines in an event and yields one delta per line, with a `[DONE]` short-circuit.
  - **Anthropic** joins all `data:` lines per spec, has no `[DONE]`, and dispatches on the SSE `event` type field (not just `data`).
  A shared helper that preserves both behaviors needs to pass `&[String]` (the data lines) to a per-provider closure, which moves most of the complexity back into the provider. Net win: ~20 lines of de-duplication at the cost of a more elaborate helper signature and stricter test coverage requirements. **Defer until a 3rd provider lands** (Gemini, DeepSeek, etc.) — at that point the helper pays for itself.
- **Splitting `openai.rs` (541 lines) and `anthropic.rs` (796 lines) into `openai/{mod,request,sse}.rs` and `anthropic/{mod,request,sse}.rs`.** These files are large because each is one provider's complete implementation; the parts are tightly coupled (wire types flow into request body, request body is what produces the bytes that get parsed back). Splitting requires `pub(super)` plumbing, test-block relocation, and careful visibility — the readability win is real but it doesn't pay for itself yet. **Defer until a 3rd provider lands**, at which point the directory split also has a real comparison case.
- **Splitting `hermes-cli/src/main.rs`.** REPL is single-responsibility; the file is short.
- **Splitting `hermes-runtime/src/lib.rs`.** `AIAgent` and `AgentOptions` are small and read together.
- **Splitting `hermes-loop/src/agent.rs`.** It is one state machine in one function; decomposition into private helpers (`run_one_iteration`, `dispatch_tool_call`) is value-positive but a separate cleanup pass.
- **Creating a new crate.** Everything stays inside existing crates.
- **Changing provider behavior.** Pure refactor for `StreamAccumulator`; pure test additions for SSE parsers.

## 3. Architecture

### 3.1 File changes (this round)

```
hermes-core/src/
  lib.rs             ← +1 line: pub mod accumulator;
  provider.rs        ← shrink to ~280 lines (Provider trait + Completion + FinishReason
                       + CompletionDelta + ToolCallDelta + their tests).
                       Re-exports StreamAccumulator + accumulate_stream
                       from accumulator.rs (see §4.2) to preserve
                       hermes_core::provider::StreamAccumulator import path
                       used by hermes-loop/src/agent.rs:186.
  accumulator.rs     ← NEW (~200 lines incl. tests). Contains:
                       - StreamAccumulator struct + Default + new
                       - add / finalize / is_empty / into_partial_message
                       - pub async fn accumulate_stream
                       - The existing accumulator test module
                         (moved verbatim from provider.rs)

hermes-providers/src/
  openai.rs          ← +tests only. Add 4 boundary tests to mod tests.
  anthropic.rs       ← +tests only. Add 4 boundary tests to mod tests.
```

### 3.2 Why test additions, not module splits, for the providers

The OpenAI and Anthropic provider files are 540 and 790 lines, but:

- Each is a single coherent provider implementation with one `Provider` impl, one request builder, one SSE parser, and one set of tests.
- The boundaries (request vs SSE) are inside the file, marked by a section comment today (`// --- Request body ---`, `// --- SSE parser ---`).
- Splitting them now would force us to define `pub(super)` boundaries and re-arrange tests — work that has no payoff until we have a 3rd provider to compare against.

The right move is to add the boundary tests now (cheap, fills real gaps), and revisit module splits the next time a provider is added.

## 4. Core changes

### 4.1 `StreamAccumulator` extraction (mechanical)

All the code below moves verbatim from `provider.rs` to `accumulator.rs`. The only change is the file location and the `use` paths.

```rust
// hermes-core/src/accumulator.rs

use std::collections::BTreeMap;

use futures::StreamExt;

use crate::error::ProviderError;
use crate::message::{Content, Message, Role, ToolCall};
use crate::provider::{Completion, CompletionDelta, FinishReason};
use crate::usage::Usage;

pub struct StreamAccumulator { /* unchanged */ }
impl Default for StreamAccumulator { /* unchanged */ }
impl StreamAccumulator {
    pub fn new() -> Self { /* unchanged */ }
    pub fn add(&mut self, delta: &CompletionDelta) { /* unchanged */ }
    pub fn finalize(self) -> Completion { /* unchanged */ }
    pub fn is_empty(&self) -> bool { /* unchanged */ }
    pub fn into_partial_message(self, role: Role) -> Message { /* unchanged */ }
}

pub async fn accumulate_stream(
    mut stream: crate::provider::CompletionStream,
) -> Result<Completion, ProviderError> { /* unchanged */ }

#[cfg(test)]
mod tests { /* unchanged — all 8 tests move with the code */ }
```

### 4.2 Public path preservation (critical)

`crates/hermes-loop/src/agent.rs:186` imports `StreamAccumulator` via the full path:

```rust
let mut acc = hermes_core::provider::StreamAccumulator::new();
```

To keep this working **without editing `agent.rs`**, `provider.rs` re-exports the moved items at the bottom of the file:

```rust
// hermes-core/src/provider.rs (new lines at the end)

// Re-exported to preserve the `hermes_core::provider::StreamAccumulator`
// import path used by `hermes-loop`. The implementation lives in
// `crate::accumulator`; `provider.rs` keeps the trait + delta types only.
pub use crate::accumulator::{accumulate_stream, StreamAccumulator};
```

`hermes-core/src/lib.rs` gains exactly one new line: `pub mod accumulator;`. No other changes.

`agent.rs` is **not** touched. The existing `hermes_core::provider::StreamAccumulator` import keeps working via the re-export.

### 4.3 SSE parser boundary tests

These go in the existing `mod tests` blocks of `openai.rs` and `anthropic.rs` (8 tests exist in OpenAI's, 9 in Anthropic's; we add 4 to each).

The 4 new tests per provider, parameterized by which provider's parser we're testing:

| Test | What it asserts |
|---|---|
| `chunks_split_across_frames_assemble_correctly` | Feed a single SSE event as **3 separate byte chunks** (e.g. `b"data: {\"ch"...`, `b"oices\":[{\"de"...`, `b"lta\":{\"content\":\"hi\"}}]}\n\n"`). The parser must produce 1 delta. Validates the buffer + `find("\n\n")` logic. |
| `transport_error_becomes_provider_error_transport` | Feed a byte stream where the underlying `Stream<Item = reqwest::Result<Bytes>>` yields `Err(reqwest::Error)`. The parser must yield `Err(ProviderError::Transport(_))` and terminate. |
| `done_sentinel_preserves_prior_deltas` | Feed 2 valid events then `data: [DONE]\n\n` then a 3rd valid event. The parser must yield 2 deltas and stop. (Anthropic: omitted because Anthropic has no `[DONE]` — replaced with an "Anthropic-specific" test, see below.) |
| `partial_utf8_in_a_chunk_does_not_panic` | Feed bytes containing invalid UTF-8 (e.g. `b"data: \xFF\xFE\n\n"`). The parser must NOT panic; it should yield either an `InvalidResponse` (for the malformed JSON) or skip the event gracefully. The current code uses `String::from_utf8_lossy`, which is fine — this test pins that behavior. |

For **Anthropic** specifically, the `[DONE]` test is replaced by:

- `message_stop_event_terminates_cleanly` — Anthropic sends an explicit `message_delta` then `message_stop`. The parser must yield the `message_delta` (carrying `finish_reason`) and then return without an extra `Ok(None)` for `message_stop`.

These are **black-box** tests: each takes a byte slice (or a stream of byte slices) and asserts on the resulting `Vec<CompletionDelta>` or first error. They do NOT depend on the helper helper (`parse_sse_for_test`) — the helper is small enough to inline.

For OpenAI, the test pattern mirrors the existing `parse_sse_for_test`:
```rust
fn parse_sse_bytes(input: &[u8]) -> Result<Vec<CompletionDelta>, ProviderError> {
    let stream = futures::stream::iter(vec![Ok::<_, reqwest::Error>(Bytes::copy_from_slice(input))]);
    let s = parse_sse_chunks(stream);
    futures::executor::block_on(async move {
        let mut v = Vec::new();
        futures::pin_mut!(s);
        while let Some(item) = s.next().await {
            v.push(item?);
        }
        Ok(v)
    })
}
```

For the **chunked** test, we need a stream of multiple byte chunks:
```rust
fn parse_sse_chunks_of(chunks: Vec<&[u8]>) -> Result<Vec<CompletionDelta>, ProviderError> {
    let stream = futures::stream::iter(
        chunks.into_iter().map(|c| Ok::<_, reqwest::Error>(Bytes::copy_from_slice(c))).collect::<Vec<_>>()
    );
    // ... same drive pattern
}
```

For the **transport error** test:
```rust
fn parse_sse_with_transport_error() -> ProviderError {
    let stream = futures::stream::iter(vec![Err(reqwest::Error::decode() /* or similar */)]);
    let s = parse_sse_chunks(stream);
    futures::executor::block_on(async move {
        let mut last_err = None;
        futures::pin_mut!(s);
        while let Some(item) = s.next().await {
            if let Err(e) = item { last_err = Some(e); break; }
        }
        last_err.expect("stream must yield at least one error")
    })
}
```

(Exact `reqwest::Error` constructor is chosen at implementation time — `reqwest::Error` doesn't have a public `new`, so the test may need a small helper that wraps a known error. If constructing a `reqwest::Error` is impractical, we can use a thin wrapper type that implements `From<...>` — but the cleanest fix is to construct one via `reqwest::Client::new().get("http://[::1]:0").send().await` and capture its error. Decide at implementation time.)

## 5. Implementation strategy (TDD per CLAUDE.md)

Per `CLAUDE.md` "TDD Workflow" — strict RED → GREEN → REFACTOR.

### Step 1: RED — write failing tests for the SSE boundary cases

For **OpenAI** (`openai.rs::mod tests`), add 4 tests:
1. `chunks_split_across_frames_assemble_correctly`
2. `transport_error_becomes_provider_error_transport`
3. `done_sentinel_preserves_prior_deltas` (already partially covered by `done_marker_terminates`, but the new test specifically asserts that the third event is **not** parsed and no extra delta is yielded)
4. `partial_utf8_in_a_chunk_does_not_panic`

For **Anthropic** (`anthropic.rs::mod tests`), add 4 tests:
1. `chunks_split_across_frames_assemble_correctly`
2. `transport_error_becomes_provider_error_transport`
3. `message_stop_event_terminates_cleanly`
4. `partial_utf8_in_a_chunk_does_not_panic`

Run `cargo test -p hermes-providers` — confirm they pass (the SSE parsers are already correct, this is test-coverage gap-filling, not a behavior change). The point of the RED phase is to make sure the new tests run before the extraction lands, so we know the extraction didn't break anything.

If any of these fail, that means the existing parser has a real bug, which is a separate finding — fix the parser minimally, document the fix, and proceed.

### Step 2: GREEN — extract `StreamAccumulator`

1. Create `hermes-core/src/accumulator.rs` and move (verbatim, except `use` paths):
   - `StreamAccumulator` struct
   - `Default` impl
   - `new`, `add`, `finalize`, `is_empty`, `into_partial_message`
   - `accumulate_stream` free function
   - The `#[cfg(test)] mod tests` block (all 8 existing tests)
2. Add `pub mod accumulator;` to `hermes-core/src/lib.rs`.
3. Add the re-export at the bottom of `provider.rs`:
   ```rust
   pub use crate::accumulator::{accumulate_stream, StreamAccumulator};
   ```
4. Delete the moved code from `provider.rs` (it now lives in `accumulator.rs`).
5. Update the `Provider::complete` default impl in `provider.rs` to call `crate::accumulator::accumulate_stream` (the path changed from the same-file call to a cross-module call).

### Step 3: Verify

```bash
cargo build
cargo test                                          # all unit + integration tests
cargo test -p hermes-core                           # focus on the changed crate
cargo test -p hermes-providers                      # focus on the changed crate
cargo clippy --all-targets --all-features -- -D warnings
```

Every test that passed before this refactor must pass after. The 8 new SSE boundary tests must pass.

### Step 4: Manual smoke (optional, requires API keys)

```bash
echo "hello" | cargo run -p hermes-cli --quiet -- --provider openai
echo "hello" | cargo run -p hermes-cli --quiet -- --provider anthropic
```

If `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` are in `.envrc`, both should produce a response — confirming the SSE parsers still work end-to-end with the test-coverage additions.

## 6. Risks and mitigations

| Risk | Mitigation |
|---|---|
| `pub use` at the bottom of `provider.rs` makes `StreamAccumulator` accessible from two paths (`hermes_core::provider::StreamAccumulator` AND `hermes_core::accumulator::StreamAccumulator`), which could be confusing | Document the canonical path in the re-export comment. The crate-root re-export at `hermes-core/src/lib.rs` is NOT added, so `hermes_core::StreamAccumulator` is **not** a new public path — only the existing `hermes_core::provider::StreamAccumulator` keeps working. |
| `reqwest::Error` is hard to construct in tests (no public `new`) | Use `reqwest::Client::new().get("http://[::1]:0/").build().unwrap().execute(request).await` to get a real error, OR abstract the stream input behind a `parse_sse_chunks_from<S: Stream<...>>` helper that takes a generic byte stream. Decide at implementation time. |
| SSE parser tests that need a `Vec<Result<Bytes, reqwest::Error>>` for chunked input require constructing multiple `reqwest::Error` values if any chunk fails | Only the transport-error test needs a real `reqwest::Error`. The chunked test only needs `Ok(Bytes)`. |
| The `use` paths inside `accumulator.rs` may need adjusting (e.g. `crate::provider::CompletionDelta` becomes `crate::provider::CompletionDelta`, which is fine since the trait + delta types stay in `provider.rs`) | The only paths that change are the `accumulate_stream` body's reference to `StreamAccumulator` (still in same file) and `Provider::complete` default impl in `provider.rs` (use `crate::accumulator::accumulate_stream`). |

## 7. Test coverage summary

**New tests** (8 total):

| Provider | Test name | What it pins |
|---|---|---|
| OpenAI | `chunks_split_across_frames_assemble_correctly` | buffer + `find("\n\n")` works across multiple byte chunks |
| OpenAI | `transport_error_becomes_provider_error_transport` | byte-stream errors propagate as `ProviderError::Transport` |
| OpenAI | `done_sentinel_preserves_prior_deltas` | events after `[DONE]` are not parsed |
| OpenAI | `partial_utf8_in_a_chunk_does_not_panic` | `String::from_utf8_lossy` behavior is stable |
| Anthropic | `chunks_split_across_frames_assemble_correctly` | (same as OpenAI, but for Anthropic's parser) |
| Anthropic | `transport_error_becomes_provider_error_transport` | (same) |
| Anthropic | `message_stop_event_terminates_cleanly` | `message_stop` is handled without yielding an extra delta |
| Anthropic | `partial_utf8_in_a_chunk_does_not_panic` | (same) |

**Existing tests, unchanged behavior**:
- `crates/hermes-core/src/provider.rs::tests` — provider-trait related tests stay.
- `crates/hermes-core/src/accumulator.rs::tests` — all 8 `StreamAccumulator` tests move with the code.
- `crates/hermes-providers/tests/openai.rs` and `tests/anthropic.rs` — integration tests, behavior unchanged.
- `crates/hermes-loop/tests/*` — exercise the loop with the providers, behavior unchanged.

## 8. Out-of-scope follow-ups (parking lot)

These were considered and deferred. Each is a separate brainstorm round, with stronger justification once a 3rd provider is on the table.

- **Shared `sse_frame_stream` helper** in `hermes-providers::sse` (or `pub(crate)`). Provider-agnostic byte-buffering. Justified when a 3rd provider lands. See §2 for the behavior-compatibility constraints.
- **Provider directory split** (`openai/{mod,request,sse}.rs`, `anthropic/{mod,request,sse}.rs`). Justified when a 3rd provider lands — at that point the cross-provider structure becomes worth formalizing.
- **Decompose `AgentLoop::run`** into private helpers (`run_one_iteration`, `dispatch_tool_call`, `append_tool_result`). Improves readability of the 200-line method.
- **`Provider::complete` default impl** could take an event callback for parity with `AgentLoop::run`'s streaming path. Separate API change, not a refactor.
