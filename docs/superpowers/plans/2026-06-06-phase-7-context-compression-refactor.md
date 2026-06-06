# Phase 7 Context Compression Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor the Phase 7 context compression implementation so the configured compressor is actually wired into the runtime, the CLI exposes `/compact [focus]`, and the compression flow matches the design closely enough to be functionally correct and testable.

**Architecture:** Keep the current `context/` module split, but move the system back toward clean boundaries: `runtime_agent` constructs and wires the compressor, `AgentLoop` owns trigger timing and event emission, and `ContextCompressor` owns compression behavior plus summary generation fallback. Add integration-style tests to prove the main paths work.

**Tech Stack:** Rust workspace, tokio async runtime, existing hermes provider abstractions, cargo test/check.

---

### Task 1: Add failing tests for wiring and manual compaction

**Files:**
- Modify: `crates/hermes-agent/src/runtime_agent.rs`
- Create: `crates/hermes-agent/tests/context_compression.rs`

- [ ] **Step 1: Write failing tests for config wiring and summary fallback expectations**
- [ ] **Step 2: Run `cargo test -p hermes-agent context_compression -- --nocapture` and confirm failures**
- [ ] **Step 3: Write minimal runtime/compressor changes to make those tests pass**
- [ ] **Step 4: Re-run `cargo test -p hermes-agent context_compression -- --nocapture` and confirm progress**

### Task 2: Refactor loop trigger flow to match the spec more closely

**Files:**
- Modify: `crates/hermes-agent/src/loop_engine.rs`
- Modify: `crates/hermes-core/src/context_engine.rs`

- [ ] **Step 1: Add failing tests for pre-turn/post-turn event behavior and skipped/error paths**
- [ ] **Step 2: Run the targeted tests and confirm they fail for the right reasons**
- [ ] **Step 3: Refactor `AgentLoop` trigger logic and engine API usage with minimal surface-area changes**
- [ ] **Step 4: Re-run targeted tests until green**

### Task 3: Add `/compact [focus]` handling and preserve clean REPL behavior

**Files:**
- Modify: `crates/hermes-agent/src/runtime_agent.rs`
- Modify: `crates/hermes-cli/src/repl.rs`
- Modify: `crates/hermes-agent/tests/context_compression.rs`

- [ ] **Step 1: Add failing tests for manual compact command behavior or the closest testable runtime entry point**
- [ ] **Step 2: Run the targeted tests and confirm failures**
- [ ] **Step 3: Implement a small runtime method / REPL dispatch path for manual compaction**
- [ ] **Step 4: Re-run targeted tests until green**

### Task 4: Clean up leftovers and run full verification

**Files:**
- Delete: `crates/hermes-agent/src/context/summary.rs.orig`
- Modify: `crates/hermes-agent/src/context/compressor.rs`
- Modify: `crates/hermes-agent/src/context/pruning.rs`
- Modify: `crates/hermes-agent/src/context/summary.rs`

- [ ] **Step 1: Remove leftover files and any dead code exposed by the refactor**
- [ ] **Step 2: Run `cargo test -p hermes-agent -- --nocapture`**
- [ ] **Step 3: Run `cargo check --workspace`**
- [ ] **Step 4: Review the spec against the final diff and note any remaining intentional gaps**
