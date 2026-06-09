# Test Fixture Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collapse scattered `for_test_*` helpers and the misnamed `App::new_for_test` constructor across the workspace, removing duplication and dead code while preserving all existing test behavior.

**Architecture:** Pure refactor. No behavior change. Each task is a self-contained change to one crate's test-plumbing, with `cargo test -p <crate>` as the safety net after every task.

**Tech Stack:** Rust 2021, standard `cargo` + `git`, mechanical `sed` for bulk renames inside single files.

---

## File Structure

This refactor touches only test-plumbing and constructors. No new files are created. No public API surface grows except for `AIAgent::new(agent_loop)` and `App::new(provider, model, max_iter, ctx_window)` — both narrow additions.

Modified files:

- `crates/hermes-cli/src/tui/app.rs` — replace `new_for_test` with `Default` + add `new(...)`
- `crates/hermes-cli/src/tui/run.rs` — collapse 5-line "build then mutate" blocks in `run` and `run_with_backend`
- `crates/hermes-cli/src/tui/{app,input,render}.rs` — `new_for_test` → `default`
- `crates/hermes-cli/tests/tui_*.rs` — `new_for_test` → `default`
- `crates/hermes-agent/src/config.rs` — delete three helpers, update `test_helpers` module comment
- `crates/hermes-agent/src/runtime_agent.rs` — replace `for_test` block with public `new(agent_loop)`
- `crates/hermes-agent/tests/common/mod.rs` — delete file
- `crates/hermes-agent/tests/context_compression.rs` — update import
- `crates/hermes-agent/tests/skills_injection.rs` — update import
- `crates/hermes-providers/src/openai/sse.rs` — collapse two SSE helpers into one

---

## Tasks

### Task 1: hermes-cli — Replace `App::new_for_test` with `Default` and add `App::new`

**Files:**
- Modify: `crates/hermes-cli/src/tui/app.rs:63-87`

- [ ] **Step 1: Delete `new_for_test` and add `Default` + `new`**

In `crates/hermes-cli/src/tui/app.rs`, replace the entire `impl App { ... new_for_test ... }` block's first method. The block currently starts at line 63 with `impl App {` and the first method is `pub fn new_for_test() -> Self` at lines 64-87.

Replace lines 64-87 with:

```rust
    /// Default state: empty scrollback, idle mode, no provider/model wired.
    /// Tests and any caller that wants to mutate fields one by one start
    /// from this. Production entry points use `App::new` instead.
    pub fn new(
        provider_name: String,
        model_name: String,
        max_iterations: u32,
        context_window_size: Option<u64>,
    ) -> Self {
        Self {
            provider_name: Some(provider_name),
            model_name: Some(model_name),
            max_iterations,
            context_window_size,
            ..Self::default()
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self {
            scrollback: Vec::new(),
            input: String::new(),
            cursor: 0,
            mode: AppMode::Idle,
            provider_name: None,
            model_name: None,
            iteration: 0,
            max_iterations: 0,
            compression_hint: None,
            turn_started_at: None,
            chat_scroll: 0,
            context_window_size: None,
            context_used_tokens: None,
            history_width: 80,
            active_turn_cancel: None,
            scrollback_revision: 0,
            cached_chat_lines: Vec::new(),
            cached_chat_width: None,
            cached_chat_revision: 0,
        }
    }
}
```

The `new` method goes inside the existing `impl App {` block. The `Default` impl is a new `impl Default for App` block that immediately follows it. Note: `new` and the closing `}` of `impl App` stay; the `Default` impl comes after the closing `}` of `impl App`.

- [ ] **Step 2: Verify the build compiles**

Run: `cargo build -p perry-hermes-cli 2>&1 | tail -20`
Expected: compile errors mentioning `App::new_for_test` is undefined. This is expected — the call sites have not been updated yet.

- [ ] **Step 3: Commit the constructor change (broken state is OK at this step)**

```bash
git add crates/hermes-cli/src/tui/app.rs
git commit -m "refactor(cli): add App::default and App::new, remove new_for_test

Mechanical follow-up in subsequent commits will update the 88 call
sites and the two production run() entry points."
```

---

### Task 2: hermes-cli — Update `run()` and `run_with_backend` to use `App::new`

**Files:**
- Modify: `crates/hermes-cli/src/tui/run.rs:77-82`
- Modify: `crates/hermes-cli/src/tui/run.rs:182-187`

- [ ] **Step 1: Update `run()` to use `App::new`**

In `crates/hermes-cli/src/tui/run.rs`, the production `run` function starts with the block:

```rust
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut app = App::new_for_test();
    app.provider_name = Some(provider_name);
    app.model_name = Some(model_name);
    app.max_iterations = max_iterations;
    app.context_window_size = context_window_size;
    let mut history = HistoryWrite::default();
```

Replace lines 78-82 (the `new_for_test` + four assignments) with a single line:

```rust
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut app = App::new(provider_name, model_name, max_iterations, context_window_size);
    let mut history = HistoryWrite::default();
```

- [ ] **Step 2: Update `run_with_backend` to use `App::new`**

The test-friendly `run_with_backend` function has the same pattern (around line 183-187). Replace its `new_for_test` + four assignments with the same single line:

```rust
    let mut app = App::new(provider_name, model_name, max_iterations, context_window_size);
```

- [ ] **Step 3: Verify the two production files compile**

Run: `cargo build -p perry-hermes-cli --bin perry-hermes-cli 2>&1 | tail -20`
Expected: compile errors from test files (the 88 call sites have not been updated). That's fine — it confirms the production code change is correct and the remaining errors are all in test files.

- [ ] **Step 4: Commit**

```bash
git add crates/hermes-cli/src/tui/run.rs
git commit -m "refactor(cli): use App::new in run and run_with_backend

Replaces 5-line 'build then mutate' blocks with a single constructor
call. Production code no longer borrows a test-named constructor."
```

---

### Task 3: hermes-cli — Bulk-rename `App::new_for_test()` → `App::default()` in test files

**Files:**
- Modify: `crates/hermes-cli/src/tui/app.rs` (unit tests in same file)
- Modify: `crates/hermes-cli/src/tui/input.rs` (unit tests)
- Modify: `crates/hermes-cli/src/tui/render.rs` (unit tests)
- Modify: All files matching `crates/hermes-cli/tests/tui_*.rs`

- [ ] **Step 1: List all files containing `App::new_for_test` after Tasks 1-2**

Run: `rg -l "App::new_for_test" --type rust 2>&1`
Expected: a list of test files only (no `run.rs` — already handled in Task 2; no `app.rs` constructor — handled in Task 1). All listed files are tests.

- [ ] **Step 2: Apply mechanical rename across all test files**

Run:

```bash
rg -l "App::new_for_test" --type rust -0 | xargs -0 sed -i '' 's/App::new_for_test()/App::default()/g'
```

(`-i ''` is the macOS `sed` form for in-place edit with no backup suffix. If running on Linux, use `sed -i` without the empty string.)

- [ ] **Step 3: Verify no `App::new_for_test` references remain**

Run: `rg "App::new_for_test" --type rust 2>&1`
Expected: no output.

- [ ] **Step 4: Run all hermes-cli tests**

Run: `cargo test -p perry-hermes-cli 2>&1 | tail -30`
Expected: all tests pass. The count of passing tests should match what was passing before this refactor.

- [ ] **Step 5: Run clippy**

Run: `cargo clippy -p perry-hermes-cli --all-targets 2>&1 | tail -20`
Expected: no new warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-cli/
git commit -m "refactor(cli): rename App::new_for_test to App::default

Mechanical rename across 88 call sites. App already had a natural
default-construction shape; the 'for_test' name was misleading and
the dedicated constructor is replaced by the std Default impl."
```

---

### Task 4: hermes-agent config — Delete three dead/default helpers, fix the comment

**Files:**
- Modify: `crates/hermes-agent/src/config.rs:313-380`

- [ ] **Step 1: Update the `test_helpers` module comment**

In `crates/hermes-agent/src/config.rs`, replace the leading `//!` doc comment of `pub mod test_helpers` (lines 314-320) with:

```rust
    //! Test fixtures — gated by `#[cfg(test)]` so they never ship in
    //! release builds. `cargo test` enables `cfg(test)` for both the
    //! library and integration-test binaries, so integration tests in
    //! `tests/` can import these helpers directly:
    //!
    //! ```ignore
    //! use perry_hermes_agent::test_helpers::PerryHermesConfig;
    //! let cfg = PerryHermesConfig::for_test_echo();
    //! ```
```

- [ ] **Step 2: Delete `PerryHermesConfig::for_test_empty`**

Delete the entire method (lines 339-341):

```rust
        pub fn for_test_empty() -> Self {
            Self::default()
        }
```

- [ ] **Step 3: Delete `PerryHermesConfig::for_test_with`**

Delete the entire method (lines 345-351):

```rust
        pub fn for_test_with(provider: ProviderConfig, agent: AgentConfig) -> Self {
            Self {
                providers: vec![provider],
                agent,
                ..Default::default()
            }
        }
```

- [ ] **Step 4: Delete `AgentConfig::for_test_default`**

Delete the entire method (lines 357-359):

```rust
        pub fn for_test_default() -> Self {
            Self::default()
        }
```

- [ ] **Step 5: Verify the lib compiles**

Run: `cargo build -p perry-hermes-agent 2>&1 | tail -20`
Expected: clean build. (The `for_test_echo` methods are still defined and still used by runtime_agent.rs and the integration tests.)

- [ ] **Step 6: Run hermes-agent tests (lib only — integration tests still use the duplicate)**

Run: `cargo test -p perry-hermes-agent --lib 2>&1 | tail -20`
Expected: all lib tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/hermes-agent/src/config.rs
git commit -m "refactor(agent): delete dead/default test helpers, fix cfg comment

- for_test_empty: pure Self::default(), no value
- for_test_with: zero callers, dead code
- for_test_default: pure Self::default(), no value
- test_helpers module comment: drop the false claim that integration
  tests can't see #[cfg(test)] items; show the direct import path"
```

---

### Task 5: hermes-agent runtime — Replace `AIAgent::for_test` with public `AIAgent::new`

**Files:**
- Modify: `crates/hermes-agent/src/runtime_agent.rs:178-186` (delete the `#[cfg(test)] impl AIAgent` block)
- Modify: `crates/hermes-agent/src/runtime_agent.rs` (add `new` to main `impl AIAgent` block, around line 27)
- Modify: `crates/hermes-agent/src/runtime_agent.rs:591` (the single call site)

- [ ] **Step 1: Add `AIAgent::new` to the main impl block**

In `crates/hermes-agent/src/runtime_agent.rs`, find the main `impl AIAgent {` block (around line 27) — it currently starts with `pub fn from_config(...)`. Insert the following method **before** `from_config` (so the naked constructor comes first, followed by the high-level wiring):

```rust
    /// Low-level constructor that takes a pre-built `AgentLoop` and an
    /// empty `system_prompt`. Production code should use
    /// `AIAgent::from_config`, which composes skills, AGENTS.md, and
    /// working-dir hints into the system prompt.
    pub fn new(agent_loop: AgentLoop) -> Self {
        Self {
            agent_loop,
            system_prompt: None,
        }
    }
```

- [ ] **Step 2: Delete the `#[cfg(test)] impl AIAgent` block**

Delete the entire block at lines 178-186:

```rust
#[cfg(test)]
impl AIAgent {
    fn for_test(agent_loop: AgentLoop) -> Self {
        Self {
            agent_loop,
            system_prompt: None,
        }
    }
}
```

- [ ] **Step 3: Update the call site**

In `crates/hermes-agent/src/runtime_agent.rs`, the single test caller (around line 591):

```rust
        let agent = AIAgent::for_test(agent_loop);
```

Change to:

```rust
        let agent = AIAgent::new(agent_loop);
```

- [ ] **Step 4: Verify build + tests**

Run: `cargo test -p perry-hermes-agent --lib 2>&1 | tail -20`
Expected: all lib tests pass. The single test that used `AIAgent::for_test` is now using the new public `AIAgent::new`.

- [ ] **Step 5: Commit**

```bash
git add crates/hermes-agent/src/runtime_agent.rs
git commit -m "refactor(agent): replace AIAgent::for_test with public AIAgent::new

The 8-line test-only impl block is gone; a 7-line public constructor
takes its place. Production code path is unchanged (still uses
AIAgent::from_config); tests now use the public, properly-named
constructor."
```

---

### Task 6: hermes-agent integration tests — Drop `tests/common/mod.rs`, point tests at `test_helpers`

**Files:**
- Delete: `crates/hermes-agent/tests/common/mod.rs`
- Modify: `crates/hermes-agent/tests/context_compression.rs:16-22`
- Modify: `crates/hermes-agent/tests/skills_injection.rs:7, 67-69`

- [ ] **Step 1: Update `tests/context_compression.rs`**

In `crates/hermes-agent/tests/context_compression.rs`, the file has:

```rust
mod common;
mod support;
use support::ScriptedProvider;
```

Remove the `mod common;` line. The `support` mod stays.

Then find the function that calls the duplicate helper:

```rust
fn echo_config_with_compression() -> PerryHermesConfig {
    common::for_test_echo()
}
```

Replace with:

```rust
fn echo_config_with_compression() -> PerryHermesConfig {
    perry_hermes_agent::test_helpers::PerryHermesConfig::for_test_echo()
}
```

- [ ] **Step 2: Update `tests/skills_injection.rs`**

In `crates/hermes-agent/tests/skills_injection.rs`, the file has `mod common;` at line 7. Remove it.

Then find:

```rust
fn config_for_echo() -> PerryHermesConfig {
    common::for_test_echo()
}
```

Replace with:

```rust
fn config_for_echo() -> PerryHermesConfig {
    perry_hermes_agent::test_helpers::PerryHermesConfig::for_test_echo()
}
```

- [ ] **Step 3: Delete `tests/common/mod.rs`**

Run: `rm crates/hermes-agent/tests/common/mod.rs`

- [ ] **Step 4: Verify no references to `common::` remain in hermes-agent tests**

Run: `rg "common::" --type rust 2>&1`
Expected: no output (we already updated both files in Steps 1-2).

- [ ] **Step 5: Run all hermes-agent tests including integration tests**

Run: `cargo test -p perry-hermes-agent 2>&1 | tail -30`
Expected: all tests pass — lib tests, the two integration test files (`context_compression.rs`, `skills_injection.rs`), and any other integration tests in the crate.

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-agent/tests/
git commit -m "refactor(agent): delete tests/common/mod.rs, use library test_helpers

The duplicate helpers were not a workaround for a Rust limitation —
cargo test enables cfg(test) for both lib and integration test
binaries, so the library's #[cfg(test)] pub mod test_helpers is
already visible. The duplicate was historical baggage."
```

---

### Task 7: hermes-providers — Collapse two-layer SSE test helper

**Files:**
- Modify: `crates/hermes-providers/src/openai/sse.rs:185-202` (delete `parse_sse_for_test`)
- Modify: `crates/hermes-providers/src/openai/sse.rs:212-214` (rename `parse_sse_bytes` and absorb body)
- Modify: 8 test call sites of `parse_sse_bytes` in the same file (lines 220, 232, 242, 254, 266, 273, 286, 308, 362, 376)

- [ ] **Step 1: Delete `parse_sse_for_test`**

In `crates/hermes-providers/src/openai/sse.rs`, delete the entire `pub(crate) fn parse_sse_for_test` block (lines 185-202, including the doc comment that starts with "/// Test helper:" and the blank line after).

- [ ] **Step 2: Rename `parse_sse_bytes` to `parse_sse_chunk` and absorb the helper's body**

The `parse_sse_bytes` function is in the `mod tests` block (around line 212):

```rust
    fn parse_sse_bytes(input: &[u8]) -> Result<Vec<CompletionDelta>, ProviderError> {
        parse_sse_for_test(input)
    }
```

Replace it with the inlined body (renamed to `parse_sse_chunk`):

```rust
    /// Drive the parser over a single byte chunk and collect every delta.
    /// `stream::iter` on a `Vec` yields a `Unpin` stream, which is what
    /// `parse_sse_chunks` expects inside `Box::pin`.
    fn parse_sse_chunk(input: &[u8]) -> Result<Vec<CompletionDelta>, ProviderError> {
        let stream =
            futures::stream::iter(vec![Ok::<_, reqwest::Error>(Bytes::copy_from_slice(input))]);
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

- [ ] **Step 3: Bulk-rename `parse_sse_bytes` → `parse_sse_chunk` across the test file**

Run: `sed -i '' 's/\bparse_sse_bytes\b/parse_sse_chunk/g' crates/hermes-providers/src/openai/sse.rs`

(The `\b` word boundaries prevent accidental matches.)

- [ ] **Step 4: Verify no `parse_sse_bytes` or `parse_sse_for_test` references remain**

Run: `rg "parse_sse_bytes|parse_sse_for_test" --type rust 2>&1`
Expected: no output.

- [ ] **Step 5: Run hermes-providers tests**

Run: `cargo test -p perry-hermes-providers 2>&1 | tail -20`
Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-providers/src/openai/sse.rs
git commit -m "refactor(providers): collapse parse_sse_for_test and parse_sse_bytes

parse_sse_for_test was a 15-line pub(crate) wrapper around
parse_sse_chunks; parse_sse_bytes was a 3-line private alias around
parse_sse_for_test. The two layers collapse to a single test-local
helper, parse_sse_chunk, with the body inlined."
```

---

### Task 8: Final verification

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace 2>&1 | tail -40`
Expected: all tests pass across all crates. No new failures vs. the baseline.

- [ ] **Step 2: Run clippy across the whole workspace**

Run: `cargo clippy --workspace --all-targets 2>&1 | tail -20`
Expected: no new warnings introduced by this refactor. (Pre-existing warnings in the codebase are out of scope.)

- [ ] **Step 3: Verify ≥ 90 LOC reduction**

Run:

```bash
git diff --stat 85543e1^..HEAD -- 'crates/*'
```

(Beginning of refactor = first commit `85543e1` (docs commit before code changes); end = current HEAD.)

Sum the changed lines. Expected: net reduction ≥ 90 lines.

If the count is short, the most likely place to look is whether `tests/common/mod.rs` (42 lines) was actually deleted — it should be visible in `git status` as no longer present.

- [ ] **Step 4: Verify forbidden symbols are gone**

Run:

```bash
rg "App::new_for_test|for_test_with|for_test_default|for_test_empty|for_test_provider_echo|tests/common|for_test_echo\\(\\)" --type rust 2>&1
```

Expected: only the surviving `PerryHermesConfig::for_test_echo()` and `ProviderConfig::for_test_echo()` calls inside `crates/hermes-agent/`. No matches in `tests/common/`, no `AIAgent::for_test`, no `parse_sse_for_test`, no `parse_sse_bytes`.

- [ ] **Step 5: Verify release build still skips test_helpers**

Run: `cargo build -p perry-hermes-agent --release 2>&1 | tail -10`
Expected: clean build. The `#[cfg(test)] pub mod test_helpers` gate ensures the module is not compiled in release mode.

- [ ] **Step 6: If a `task-8-final` commit is needed, do it; otherwise just summarize**

If Steps 1-5 all pass and there are no further changes, the refactor is complete. The final commit on the branch is the summary.
