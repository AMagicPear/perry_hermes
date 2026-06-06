# Phase 7 — Context Compression

**Date:** 2026-06-06
**Status:** Proposed
**Supersedes:** the one-line "Context compression (Phase 7)" entry in `CLAUDE.md` "Still open" / "P1 (next up)" and the placeholder in `hermes-agent/src/loop_engine.rs:LoopConfig`
**Reference implementations (read-only, not ported verbatim):**
- `/Users/amagicpear/.hermes/hermes-agent/agent/context_compressor.py` (2078 lines) — Python design source-of-truth
- `/Users/amagicpear/.hermes/hermes-agent/agent/context_engine.py` (226 lines) — Python `ContextEngine` ABC
- `/Users/amagicpear/.hermes/hermes-agent/agent/conversation_compression.py` (785 lines) — Python orchestrator
- `https://github.com/openai/codex` `codex-rs/core/src/compact.rs` (617 lines) + `compact_remote_v2.rs` (804 lines) — secondary reference for the trim-first / InitialContextInjection ideas

> **Design intent:** follow the Python `ContextCompressor` algorithm (cheap pre-pass → head/tail protection → LLM summary → iterative update) and Python's anti-thrashing discipline, but **deliberately drop** the heavy infrastructure Python carries (SQLite session lock, separate aux-client wiring, image shrinking, LCM/DAG engine ABC, 600s failure cooldown, static fallback summary, focus_topic hand-off to memory). Borrow only two things from Codex: the `InitialContextInjection` enum and the trim-function-outputs pre-pass. End state: **~1000–1300 LOC total**, vs Python's ~3800 LOC.

---

## 1. Goal

When conversation history approaches the model's context limit, compress the middle turns via an LLM-generated summary while preserving the head (system prompt + earliest user/assistant exchange) and the tail (most recent ~20K tokens of user messages). The agent loop emits a `LoopEvent::CompressionCompleted` event so the CLI and future gateway can render the action.

After this phase, long sessions no longer grow without bound. Long-running REPL sessions, multi-day agent tasks, and Phase 11's gateway subagents can all complete without the provider rejecting the payload for context overflow.

---

## 2. Non-Goals

- Pluggable engine ABC (LCM, DAG, etc.) — single `ContextCompressor` impl only. The trait is a Rust trait, not a Python-style ABC; plugin authors can add engines later.
- Server-side / remote compaction (Codex v2 path) — local only.
- `IterationBudget` refund/grace-call semantics for subagents (Phase 11).
- `MemoryManager::on_pre_compress` hook — Phase 12 (curator).
- `ContextWindowExceeded` image-shrink recovery — Phase 8 follow-up.
- `@-reference` parser (Python `context_references.py`) — Rust has no equivalent.
- tiktoken-rs integration — `chars / 4` is good enough for the first version; add a `tiktoken` feature flag in a follow-up.
- 600s summary-failure cooldown + static fallback summary — Codex's "drop oldest message and retry" is simpler and equally effective.
- `ToolContext.permissions` enforcement of compression actions (compression is internal to the loop, not a tool call).
- `disabled_toolsets` reactive to per-turn changes — separate P1.
- "Context compression" message in the system prompt — system prompt never references compression; compression is transparent to the model.

---

## 3. Trigger Strategy

Three trigger points, evaluated in order. All return the same `Vec<Message>` from the engine.

| # | Where | Signal | Action |
|---|---|---|---|
| 1 | **Pre-turn** (start of every `run()`) | estimated prompt tokens ≥ `threshold_tokens` (default 50% of model context) | Compress, then proceed with normal turn |
| 2 | **Post-turn** (after every API response) | `usage.input_tokens ≥ threshold_tokens` | Compress, then dispatch the tool calls in the same iteration |
| 3 | **Manual** (`/compact [focus]`) | user slash command | Compress, then continue; `focus` becomes `focus_topic` |

`threshold_tokens` is computed once at engine init: `max(int(context_length * threshold_percent), MINIMUM_CONTEXT_LENGTH)`. Default `threshold_percent = 0.50` (Python default). The user can override in TOML via `[agent] context_compression_threshold_percent = 0.50`.

If `should_compress` is true but the last two compressions each saved < 10% of tokens, the trigger is suppressed and a warning is emitted. This is the Python `_ineffective_compression_count` backoff.

---

## 4. `ContextEngine` Trait

Single trait, no ABC factory, no plugin registry. Lives in `crates/hermes-core/src/context_engine.rs`.

```rust
pub trait ContextEngine: Send + Sync {
    /// One-line identifier (e.g. "compressor"). Used in telemetry and logs.
    fn name(&self) -> &'static str;

    /// Update tracked token usage from the latest API response.
    /// Called once after every successful completion (preflight and postflight).
    fn update_from_response(&mut self, usage: &Usage);

    /// Cheap pre-call check. Default impl returns `false` (no preflight).
    /// The default `ContextCompressor` overrides this with a rough token estimate.
    fn should_compress(&self) -> bool { false }

    /// Heavy entry point. Returns the new (possibly shorter) message list.
    /// Implementations must preserve:
    ///   - system prompt (always)
    ///   - first `protect_first_n` non-system messages (head)
    ///   - last `protect_last_n` messages / last 20K tokens of user messages (tail)
    /// `focus_topic` is `Some(_)` for `/compress <focus>`, `None` otherwise.
    /// `current_tokens` is the rough pre-call estimate when known.
    fn compress(
        &mut self,
        messages: Vec<Message>,
        current_tokens: Option<u64>,
        focus_topic: Option<&str>,
    ) -> Result<Vec<Message>, CompressError>;

    /// Called when `/new` or `/reset` is invoked. Reset per-session state.
    fn on_session_reset(&mut self);

    /// Called when the model or context length changes.
    fn update_model(&mut self, model: &str, context_length: u64);
}
```

`CompressError` is a new variant in `hermes_core::error`:
```rust
pub enum CompressError {
    /// LLM summary call failed; oldest message was already dropped and retry also failed.
    /// Caller should treat this as a fatal error for the current turn.
    SummaryFailed(String),
    /// No messages eligible for compression (everything is protected).
    NothingToCompress,
}
```

---

## 5. `ContextCompressor` Default Implementation

The single built-in engine. Lives in `crates/hermes-agent/src/context/compressor.rs`. Follows the Python 5-step algorithm with the simplifications listed in §10.

### 5.1 Configuration

```rust
pub struct CompressorConfig {
    pub threshold_percent: f64,         // default 0.50
    pub protect_first_n: usize,         // default 3
    pub protect_tail_tokens: u64,       // default 20_000 (Codex constant)
    pub summary_target_ratio: f64,      // default 0.20 — summary is sized to 20% of threshold
    pub chars_per_token: f64,           // default 4.0
    pub min_summary_tokens: u64,        // default 2_000
    pub max_summary_tokens: u64,        // default 12_000
    pub ineffective_threshold: f64,     // default 0.10 — back off if last 2 compressions saved < 10%
    pub max_drop_on_failure: usize,     // default 1 — Codex: drop oldest message and retry once
}

pub struct ContextCompressor {
    config: CompressorConfig,
    model: String,
    context_length: u64,
    threshold_tokens: u64,
    /// Last LLM-generated summary; reused as the seed for the next compression.
    previous_summary: Option<String>,
    /// Tokens saved by the most recent compression (0.0 = nothing saved).
    last_savings_ratio: f64,
    /// Count of consecutive compressions that saved < ineffective_threshold.
    ineffective_count: u32,
    /// Aux provider for summary calls. None → fall back to the main provider.
    summary_provider: Option<Arc<dyn Provider>>,
    /// Per-session compression lock.
    lock: Arc<tokio::sync::Mutex<()>>,
}
```

### 5.2 The 5-step Algorithm

Implemented as `ContextCompressor::compress()`:

```rust
fn compress(&mut self, messages: Vec<Message>, current_tokens: Option<u64>, focus_topic: Option<&str>) -> Result<Vec<Message>, CompressError> {
    // Step 1: cheap pre-pass — replace old tool result contents with one-line summaries.
    //   Iterates oldest → newest, skips the last `protect_tail_tokens` worth.
    //   Tool results older than the tail become "[Old tool output cleared, {tool_name} returned N bytes]".
    //   No LLM call. If the pre-pass alone brings estimated tokens below threshold, return early.
    let (messages, _pruned) = self.prune_old_tool_results(&messages);

    // Re-estimate. If we're under threshold, no LLM call needed.
    let est = self.estimate_tokens(&messages);
    if est < self.threshold_tokens {
        return Ok(messages);
    }

    // Step 2: head protection — find the index after the first `protect_first_n` non-system messages.
    //   System prompt is always index 0 and is always preserved.
    let head_end = self.find_head_boundary(&messages);

    // Step 3: tail protection — walk backwards from the end, accumulating tokens up to
    //   `protect_tail_tokens`. If a message doesn't fit, truncate its text content.
    let tail_start = self.find_tail_cut_by_tokens(&messages, head_end);

    // Step 4: LLM summary of the middle slice.
    //   If `self.previous_summary` is Some, the LLM is asked to UPDATE it with the new middle;
    //   otherwise it generates from scratch.
    //   The aux provider (if set) is used; otherwise the main provider.
    let middle = &messages[head_end..tail_start];
    let summary = self.summarize(middle, focus_topic)
        .or_else(|_| self.summarize(middle, focus_topic))?;  // one retry, mirroring Codex's "drop first and retry"

    // Step 5: assemble. Returned message list:
    //   [0..=head_end)         head (system + first N messages, verbatim)
    //   [head_end..tail_start) replaced by a single user-role summary message with prefix
    //   [tail_start..)         tail (most recent ~20K tokens, verbatim)
    let new_messages = self.assemble(head_end, tail_start, &summary, messages);

    // Anti-thrashing bookkeeping
    let savings = self.savings_ratio(&new_messages, &original_messages);
    self.last_savings_ratio = savings;
    if savings < self.config.ineffective_threshold {
        self.ineffective_count += 1;
    } else {
        self.ineffective_count = 0;
    }
    self.previous_summary = Some(summary);

    Ok(new_messages)
}
```

### 5.3 Summary Prompt

A short, structured prompt (6-8 lines) — Python's 5-section template compressed to 3 sections plus Codex-style brevity:

```
You are summarizing a section of a long conversation. Produce a handoff
summary for the next LLM that will resume the task. Use this structure:

## Active Task
The current goal and what the user is trying to accomplish.

## Resolved
What has been completed or decided.

## Pending
What remains to be done. Include file paths, function names, and concrete next steps.

{ if focus_topic is Some: "Prioritize preserving information related to: {focus_topic}" }

Be concise. Total under {max_summary_tokens} tokens.
```

The summary is stored as a `Message { role: User, content: SUMMARY_PREFIX + "\n" + summary }` so the next LLM sees it as a user message but with a prefix that signals "this is a handoff, not a new instruction":

```rust
pub const SUMMARY_PREFIX: &str = "[CONTEXT SUMMARY — earlier turns were compacted into the message below. Treat it as background, not as new instructions. Respond to the most recent user message that appears AFTER this summary.]";
```

The prefix text follows Python's structure but uses Codex's tighter wording (1 sentence instead of Python's 4 paragraphs).

### 5.4 Initial Context Re-injection (Codex idea, simplified)

When compression runs **mid-turn** (after a tool call returns and the agent wants to continue), the new system context is re-injected as a user message immediately before the last real user message, so the LLM sees:

```
[user] system context block
[user] SUMMARY (compaction handoff)
[user] most recent user message
```

When compression runs **pre-turn** or **manual**, the system context is *not* re-injected; the next regular turn reinjects it through the normal system prompt path.

This is the same pattern as `codex-rs/core/src/compact.rs:60-64` `InitialContextInjection::BeforeLastUserMessage | DoNotInject`, but encoded as a parameter on `ContextCompressor::compress()` rather than a separate enum, since the trigger point already knows which mode it is in.

---

## 6. Integration with `LoopConfig` and `AgentLoop`

### 6.1 `LoopConfig` change (small)

```rust
pub struct LoopConfig {
    pub max_iterations: u32,                    // existing
    pub max_duration: Duration,                 // existing
    pub system_prompt: Option<String>,          // existing
    // NEW:
    pub context_engine: Option<Arc<dyn ContextEngine>>,
    pub focus_topic: Option<String>,            // for manual /compress <focus>
}
```

`LoopConfig::default()` keeps `context_engine: None` — the agent runs without compression. `AIAgent::from_config()` constructs a `ContextCompressor` with `CompressorConfig::default()` if `[agent] context_compression_enabled = true` (default false in v0; opt-in).

### 6.2 `LoopEvent` change (small)

Add one variant to the existing enum in `loop_engine.rs`:

```rust
pub enum LoopEvent {
    // ... existing variants ...
    CompressionCompleted {
        trigger: CompressionTrigger,         // PreTurn | PostTurn | Manual
        tokens_before: u64,
        tokens_after: u64,
        summary_chars: usize,
        duration: Duration,
    },
    CompressionSkipped { reason: CompressionSkipReason },
}

pub enum CompressionTrigger { PreTurn, PostTurn, Manual }
pub enum CompressionSkipReason { Ineffective, NothingToCompress, Disabled }
```

CLI renders `CompressionCompleted` as a one-line status (`🗜️ Compressed: 142K → 38K tokens in 1.2s`). Future gateway forwards it as a status event.

### 6.3 `AgentLoop::run` integration (two call sites)

```rust
// At the top of run(), before the main loop:
if let Some(engine) = &self.config.context_engine {
    if engine.should_compress() {
        if let Some(event) = self.compress(engine, CompressionTrigger::PreTurn, &mut messages).await? {
            self.emit_event(event);  // yields CompressionCompleted or CompressionSkipped
        }
    }
}

loop {
    // ... existing stream → dispatch cycle ...
    
    // After every successful API response, BEFORE the next iteration:
    if let Some(engine) = &self.config.context_engine {
        engine.update_from_response(&completion.usage);
        if engine.should_compress() {
            if let Some(event) = self.compress(engine, CompressionTrigger::PostTurn, &mut messages).await? {
                self.emit_event(event);
            }
        }
    }
}
```

The `self.compress(...)` helper wraps the engine call with the `tokio::sync::Mutex` per-session lock and a `LoopMetrics` increment.

### 6.4 CLI slash command

Add `/compact [focus]` to `crates/hermes-cli/src/main.rs` slash-command dispatch. On match, the CLI calls `runtime.run_compact(focus)` which triggers the same compress path with `CompressionTrigger::Manual` and `focus_topic: Some(focus)`. This mirrors Python's `/compress <focus>` and Codex's `/compact`.

---

## 7. Concurrency

A single `tokio::sync::Mutex` per `AIAgent` instance, held only during `compress()`. No SQLite, no cross-process lock — Phase 7 does not need it because:

- The current runtime runs a single agent per CLI process.
- Future gateway (Phase 11) will construct per-session `AIAgent` instances, each with its own `Mutex`. Cross-session lock is not needed because sessions are independent.
- The `background_review` parent/fork race that Python's SQLite lock solves does not exist in Rust yet (no fork in the runtime today).

The lock is acquired by `AgentLoop::compress()` before calling the engine. If another `compress()` is already running (shouldn't happen in single-agent mode, but defensive), the second caller skips with `CompressionSkipReason::NothingToCompress` — better than blocking the event loop.

```rust
async fn compress(
    &self,
    engine: &Arc<dyn ContextEngine>,
    trigger: CompressionTrigger,
    messages: &mut Vec<Message>,
) -> Result<Option<LoopEvent>, LoopError> {
    let _guard = match self.compression_lock.try_acquire() {
        Ok(g) => g,
        Err(_) => {
            return Ok(Some(LoopEvent::CompressionSkipped {
                reason: CompressionSkipReason::NothingToCompress,
            }));
        }
    };
    // ... invoke engine, measure duration, emit event ...
}
```

---

## 8. Token Estimation

`chars / 4` everywhere. No tiktoken. Add a `tiktoken` feature flag in a follow-up spec if precision proves necessary. The estimate is used in two places:

1. `ContextCompressor::should_compress()` — preflight rough check before the next API call.
2. `ContextCompressor::estimate_tokens()` — post-step-1 re-check, also used for anti-thrashing accounting.

The post-turn trigger uses the **real** `usage.input_tokens` from the API response, which is exact. The pre-turn and post-step-1 estimates are rough by design; the threshold has ~10% headroom to absorb the noise.

---

## 9. Failure Modes

Three failure modes, each handled simply.

| Failure | Detection | Response |
|---|---|---|
| LLM summary call errors (network / 5xx) | `summarize()` returns `Err` | Retry once. If still failing, **drop the oldest message from history and re-attempt the whole compress** (Codex pattern, `compact.rs:251-266`). If that still fails, return `CompressError::SummaryFailed` and let the loop surface it to the user. |
| Compression saves < 10% of tokens twice in a row | `ineffective_count >= 2` | Skip future compressions until `/new` or `/reset`. Emit `LoopEvent::CompressionSkipped { reason: Ineffective }` so the CLI can render a hint. |
| Nothing eligible to compress (everything is protected) | `head_end >= tail_start` | Return `CompressError::NothingToCompress`. Loop emits `CompressionSkipped` and continues without compression. |

There is no 600s cooldown, no static fallback summary, no aux-model-feasibility check at startup. The aux provider (if configured) failing is the same as the main provider failing.

---

## 10. What This Spec Drops From Python (deliberate simplifications)

Recorded so the next phase that revisits compression can recover them.

| Dropped | Python reference | Reason |
|---|---|---|
| `ContextEngine` ABC factory + plugin registry | `context_engine.py:42-225` | Single impl is enough. Trait is enough; no need for an `add_provider`-style registry. |
| SQLite-backed per-session lock | `conversation_compression.py:336-431` | `tokio::sync::Mutex` per `AIAgent` covers Phase 7 and Phase 11. |
| `MemoryManager::on_pre_compress` hook | `memory_manager.py:463-484` | No `MemoryStore` trait in Rust yet. Phase 12. |
| `try_shrink_image_parts_in_messages` recovery | `conversation_compression.py:617-660` | Phase 8 Anthropic follow-up. |
| 600s summary-failure cooldown | `context_compressor.py:_SUMMARY_FAILURE_COOLDOWN_SECONDS = 600` | Codex's "drop oldest and retry" is simpler and works. |
| Static fallback summary | `context_compressor.py:_build_static_fallback_summary` | Same reason. |
| Auxiliary model feasibility check at startup | `conversation_compression.py:64-251` | Aux is opt-in via runtime construction; no TOML surface for it in Phase 7. |
| `should_defer_preflight_to_real_usage` anti-noise | `context_compressor.py:698-726` | Threshold has 10% headroom; the noise doesn't matter at 50% threshold. |
| `has_content_to_compress` preflight | `context_engine.py:103-114` | The first estimate inside `compress()` is cheap enough. |
| `focus_topic` hand-off to memory provider | Python CLI `/compress <focus>` → memory | Phase 12. |
| `try_shrink_image_parts_in_messages` (image-too-large recovery) | `conversation_compression.py:617-660` | Phase 8 follow-up. |
| Plugin/LCM/DAG engine | `context_engine.py` (LCM in plugins/) | Out of scope. |
| 5-section structured summary template (Resolved/Pending/Active Task/In Progress/Remaining Work) | `context_compressor.py:SUMMARY_PREFIX` | Collapsed to 3 sections (Active Task / Resolved / Pending) per §5.3. |
| Manual `try_acquire_compression_lock` SQLite migration logic | `conversation_compression.py:385-407` | Rust `Mutex` doesn't need migration. |
| Streaming token scrubber (`sanitize_context`, `StreamingContextScrubber`) | `memory_manager.py:54-225` | Unrelated to compression. |

---

## 11. What This Spec Borrows From Codex (where it simplifies Python)

| Borrowed | Codex reference | Why it simplifies |
|---|---|---|
| `COMPACT_USER_MESSAGE_MAX_TOKENS = 20_000` constant | `compact.rs:49` | Python uses `protect_last_n: 20` (message count); a token cap is more predictable. |
| Trim function-call-outputs pre-pass | `compact_remote.rs:409-449` | Cleaner than Python's per-tool result pruning — one place, no per-tool knowledge. |
| Summary as a user message with one-line prefix | `compact.rs:292` + `prompts/templates/compact/summary_prefix.md` | Avoids Python's 4-paragraph prefix that's prone to "self-contradicting resume exactly" bugs (see Python `_HISTORICAL_SUMMARY_PREFIXES`). |
| Drop-oldest-and-retry on summary failure | `compact.rs:251-266` | Replaces Python's 600s cooldown + static fallback. |
| `InitialContextInjection` enum (BeforeLastUserMessage / DoNotInject) | `compact.rs:60-64` | Solves the "orphan tool block" problem (`CLAUDE.md` P1, also called out in `2026-06-05-phase-8-anthropic-provider-design.md:432`). |
| `LoopEvent::CompressionCompleted` analytics variant | `compact.rs:326-392` (`CompactionAnalyticsAttempt`) | Visible in CLI and forwardable to gateway. |
| `model_auto_compact_token_limit` config-driven threshold | `openai_models.rs:430-441` | TOML override instead of a code constant. |

---

## 12. File Layout

```text
crates/hermes-core/src/
├── context_engine.rs       # NEW: ContextEngine trait + CompressError + CompressionTrigger/SkipReason
└── error.rs                # MODIFIED: add CompressError variant (or new error.rs file)

crates/hermes-agent/src/
├── context/                # NEW module
│   ├── mod.rs
│   ├── compressor.rs       # ContextCompressor + CompressorConfig
│   ├── summary.rs          # Summary prompt + SUMMARY_PREFIX + iterative update
│   └── pruning.rs          # Old tool-result pruning + trim-by-tokens
├── loop_engine.rs          # MODIFIED: add pre/post-turn compress call sites + LoopEvent variants
├── lib.rs                  # MODIFIED: pub mod context;
└── runtime_agent.rs        # MODIFIED: AIAgent::from_config wires a default ContextCompressor

crates/hermes-cli/src/
└── main.rs                 # MODIFIED: /compact [focus] slash command
```

---

## 13. LOC Budget

| Component | LOC | Notes |
|---|---:|---|
| `ContextEngine` trait + `CompressError` + `CompressionTrigger` / `CompressionSkipReason` enums | ~120 | `context_engine.rs` |
| `ContextCompressor` + `CompressorConfig` | ~500 | 5-step algorithm + anti-thrashing |
| `summary.rs` (prompt + iterative update) | ~120 | 3-section template |
| `pruning.rs` (cheap pre-pass + trim-by-tokens) | ~150 | |
| `LoopConfig` + `LoopEvent` additions | ~50 | |
| `AgentLoop::run` integration (2 call sites) | ~120 | |
| `AIAgent::from_config` wiring | ~30 | |
| CLI `/compact` slash command | ~50 | |
| **Total production code** | **~1140** | |
| Tests (`tests/context_compression.rs`) | ~500 | Algorithm tests + ScriptedProvider integration |
| **Grand total** | **~1640** | |

Vs. Python: ~3800 LOC. ~57% reduction. The reduction comes entirely from the §10 drops, not from cutting algorithm steps.

---

## 14. Testing Strategy

All tests live in `crates/hermes-agent/tests/context_compression.rs`. The existing `ScriptedProvider` in `crates/hermes-agent/tests/support/` is reused; a new `MockSummaryProvider` (sibling fixture) returns canned summaries for multi-compression scenarios.

| Test | Verifies |
|---|---|
| `pre_pass_alone_keeps_under_threshold` | Cheap pre-pass on a 200-message transcript with 50 tool outputs returns early without any LLM call. |
| `llm_summary_called_when_pre_pass_insufficient` | Transcript with 100 tool outputs each containing 5KB → pre-pass + LLM summary. `MockSummaryProvider` called exactly once. |
| `head_and_tail_protected` | Result message list contains: original system message, first 3 non-system messages, summary message, last 20K tokens of user messages. |
| `anti_thrashing_skips_after_two_ineffective_compactions` | Two compressions each saving < 10% → third `should_compress()` returns false. |
| `drop_oldest_retry_on_summary_failure` | `MockSummaryProvider` fails twice → `compress()` returns the original messages minus the first, no infinite loop. |
| `manual_compress_with_focus_topic` | `/compress task-X` includes the `Prioritize preserving information related to: task-X` sentence in the summary prompt sent to `MockSummaryProvider`. |
| `iterative_summary_update` | Second compression's prompt contains the first compression's summary text. |
| `summary_prefix_attached` | The user message replacing the middle has `SUMMARY_PREFIX` as a prefix. |
| `concurrent_compress_skipped` | Two parallel `compress()` calls; second is `CompressionSkipped`. |
| `loop_emits_compression_completed_event` | `AgentLoop::run` with a `ScriptedProvider` that returns 90% context usage → `LoopEvent::CompressionCompleted` is emitted with the right `tokens_before` / `tokens_after`. |
| `loop_emits_compression_skipped_when_disabled` | `LoopConfig { context_engine: None }` → no compression events. |
| `summary_replaces_only_middle` | Final message list is `[head, summary, tail]` — never more than one summary message. |

Plus: `cargo clippy --all-targets -- -D warnings` clean, doc-tests pass.

---

## 15. Implementation Steps (TDD)

1. **RED:** Write `tests/context_compression.rs::pre_pass_alone_keeps_under_threshold`. Confirm it fails.
2. **GREEN:** Implement `pruning.rs::prune_old_tool_results`. Test passes.
3. **RED:** `tests/...::llm_summary_called_when_pre_pass_insufficient`. Add `MockSummaryProvider` fixture.
4. **GREEN:** Implement `compressor.rs::ContextCompressor::compress()` steps 1–3 (no LLM yet — return early with the protected head/tail only). Test passes (it now reports a no-op summary, not a real one).
5. **RED:** `tests/...::head_and_tail_protected` + `tests/...::summary_prefix_attached`.
6. **GREEN:** Implement steps 4–5 with a real `MockSummaryProvider`. Tests pass.
7. **RED:** `tests/...::anti_thrashing_skips_after_two_ineffective_compactions`.
8. **GREEN:** Add `_ineffective_count` and `should_compress()` check. Test passes.
9. **RED:** `tests/...::drop_oldest_retry_on_summary_failure` + `tests/...::iterative_summary_update`.
10. **GREEN:** Add retry path and `previous_summary` propagation. Tests pass.
11. **REFACTOR:** Extract `summary.rs` and the `assemble` helper. Tests still green.
12. **RED:** `tests/...::loop_emits_compression_completed_event`.
13. **GREEN:** Wire into `AgentLoop::run` pre/post-turn. Test passes.
14. **GREEN:** CLI `/compact` slash command. Manual smoke test.
15. **GREEN:** `AIAgent::from_config` default wiring when `context_compression_enabled = true`.
16. **REFACTOR:** Document the `LoopEvent::CompressionCompleted` render path in CLI.

Total estimated effort: ~3 days of focused TDD.

---

## 16. Follow-Up Backlog (deferred, not part of this spec)

These are known P1/P2 items that the user may revisit in a future spec:

- P1: tiktoken-rs feature flag for accurate token estimation
- P1: `MemoryManager::on_pre_compress` hook (Phase 12 prerequisite)
- P1: `IterationBudget` for subagent delegation (Phase 11 prerequisite)
- P1: 600s summary-failure cooldown + static fallback (if real users hit thrashing on weak models)
- P2: Server-side / remote compaction (Codex v2 path)
- P2: Pluggable engine ABC for LCM / DAG alternatives
- P2: Aux model TOML config (`[provider.summary]`)
- P2: 5-section structured summary template (current 3 sections are enough for v0)
- P2: `should_defer_preflight_to_real_usage` anti-noise for schema-heavy tool calls
- P2: Image shrinking recovery for `ContentWindowExceeded` errors during compact itself
- P2: `focus_topic` influence on summary-prompt temperature / reasoning effort

---

## 17. CLAUDE.md Updates (after merge)

- "Known Issues / Still open / Context compression (Phase 7)" → mark ✅.
- "Known Issues / P1 (next up) / No `IterationBudget`" → leave; that is Phase 11 scope, not Phase 7.
- "Current progress" line → `**Phase 0–9, Phase 7** completed ...`.
- Architecture diagram → add a tiny `context/` module under `hermes-agent` if shown.

---

## 18. Decision Points for User (before implementation)

These were settled in this spec but worth a sanity check:

1. **Threshold default 50%** — Python default. Codex uses 90% (with BodyAfterPrefix semantics). I picked 50% because Rust has no BodyAfterPrefix scope in v0 (single Total scope) and 50% is safer for weak / small models. OK?
2. **Aux provider is opt-in** — `ContextCompressor` takes `Option<Arc<dyn Provider>>` for the summary model. If `None`, uses the main provider. No TOML config in v0. OK?
3. **Compression is opt-in** — `LoopConfig::default()` keeps `context_engine: None`. CLI / config needs `[agent] context_compression_enabled = true` to enable. Alternative: always-on, default `CompressorConfig`. I prefer opt-in for the first merge (less surprise to existing users). OK?
4. **No tiktoken-rs in v0** — `chars / 4` only. Feature flag later. OK?
5. **No `/compress <focus>` influence on temperature / reasoning** — the focus only changes the prompt text. OK?

If you want any of these flipped, say so before Step 1; otherwise implementation can start.
