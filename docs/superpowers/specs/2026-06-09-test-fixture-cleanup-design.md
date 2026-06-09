# Test Fixture Cleanup Design

**Date:** 2026-06-09
**Project:** `perry_hermes`
**Goal:** Reduce scattered, duplicated test-fixture construction code across the workspace, eliminate code smells where production code uses test-named constructors, and lower total LOC without losing functionality.

## Context

The workspace has accumulated two distinct "test fixture" patterns that have drifted into code smell territory:

1. `App::new_for_test()` in `hermes-cli` is called 88 times across unit tests, integration tests, **and two production `run()` entry points**. The name claims "for test" but the function is the de-facto App constructor. There is no `Default` impl, so every caller mechanically writes `let mut app = App::new_for_test();` and then mutates individual fields.

2. `for_test_echo` (and friends) for `PerryHermesConfig` and `ProviderConfig` is implemented **twice** — once in `crates/hermes-agent/src/config.rs` under `#[cfg(test)] pub mod test_helpers`, and once in `crates/hermes-agent/tests/common/mod.rs`. The duplication is rationalized by a comment that claims integration tests cannot see `#[cfg(test)]` items, which is incorrect: `cargo test` enables `cfg(test)` for both library and integration test crates, so integration tests **can** see the library's `#[cfg(test)] pub mod`. The duplication is historical baggage.

A few small helpers (`AIAgent::for_test`, `parse_sse_for_test`, `AgentConfig::for_test_default`) are also implicated. Some are kept as-is, some are deleted as pure boilerplate.

## Current State

| Pattern | Location | Count | Issue |
| --- | --- | --- | --- |
| `App::new_for_test()` call sites | `hermes-cli` | 88 | Misnamed; used in production; no `Default` impl |
| `App::new_for_test()` definition | `hermes-cli/src/tui/app.rs:65-87` | 1 | 22 lines of field assignments |
| `PerryHermesConfig::for_test_echo` (lib) | `hermes-agent/src/config.rs:326-336` | 1 | Real logic |
| `ProviderConfig::for_test_echo` (lib) | `hermes-agent/src/config.rs:365-378` | 1 | Real logic |
| `PerryHermesConfig::for_test_empty` | `hermes-agent/src/config.rs:339-341` | 1 | Pure `Self::default()` — boilerplate |
| `PerryHermesConfig::for_test_with` | `hermes-agent/src/config.rs:345-351` | 1 | Real logic |
| `AgentConfig::for_test_default` | `hermes-agent/src/config.rs:357-359` | 1 | Pure `Self::default()` — boilerplate |
| `for_test_echo` (integration duplicate) | `hermes-agent/tests/common/mod.rs:17-27` | 1 | Duplicate of lib helper |
| `for_test_provider_echo` (integration duplicate) | `hermes-agent/tests/common/mod.rs:29-42` | 1 | Duplicate of lib helper |
| `AIAgent::for_test(agent_loop)` | `hermes-agent/src/runtime_agent.rs:180-185` | 1 | Private, 1 caller — kept as-is |
| `parse_sse_for_test(input)` | `hermes-providers/src/openai/sse.rs:188` | 1 | Single call site — kept as-is |

## Design

### Part 1 — `hermes-cli`: `App::default()` + `App::new(...)`, drop `new_for_test`

**`crates/hermes-cli/src/tui/app.rs`:**

- Remove `pub fn new_for_test() -> Self` (lines 65-87, 22 lines).
- Add `impl Default for App` whose body is the same field-init block (13 lines including `impl`/`fn`/closing brace). Every test call site changes from `App::new_for_test()` to `App::default()`.
- Add `pub fn new(provider_name: String, model_name: String, max_iterations: u32, context_window_size: Option<u64>) -> Self` (7 lines) that takes the four fields that `run()` and `run_with_backend` currently set by hand.

**`crates/hermes-cli/src/tui/run.rs`:**

- In `pub async fn run(...)` (around line 78-82), replace the 5-line "build then mutate" block with one line:
  ```rust
  let mut app = App::new(provider_name, model_name, max_iterations, context_window_size);
  ```
- In `pub async fn run_with_backend(...)` (around line 183-187), do the same.

**Test files (`hermes-cli/src/tui/{app,input,render}.rs` and `hermes-cli/tests/tui_*.rs`):**

- Mechanical replacement: `App::new_for_test()` → `App::default()`. Field-assignment lines that follow each `let mut app = ...` stay as-is — collapsing them into a builder is out of scope.

### Part 2 — `hermes-agent`: de-duplicate `for_test_echo`, delete boilerplate helpers

**`crates/hermes-agent/src/config.rs`:**

- **No `cfg` change required.** Keep the existing `#[cfg(test)] pub mod test_helpers` gate. `cargo test` sets `cfg(test)` for both the library and integration test binaries, so integration tests **already** have access to the library's `#[cfg(test)] pub mod`. The existing `tests/common/mod.rs` is redundant — it is not a workaround for a Rust limitation, it is historical duplication.
- Update the leading `test_helpers` module comment (currently lines 314-320) to remove the false claim that integration tests cannot see `cfg(test)` items, and to point integration tests at `perry_hermes_agent::test_helpers::*` directly.
- Delete `AgentConfig::for_test_default()` (lines 357-359): pure `Self::default()`, no value.
- Delete `PerryHermesConfig::for_test_empty()` (lines 339-341): pure `Self::default()`, no value.
- Keep `PerryHermesConfig::for_test_echo()`, `PerryHermesConfig::for_test_with()`, and `ProviderConfig::for_test_echo()` — each contains real field construction logic.

**`crates/hermes-agent/tests/common/mod.rs`:**

- Delete the entire file. The two integration test consumers (`tests/context_compression.rs`, `tests/skills_injection.rs`) change `common::for_test_echo()` to `perry_hermes_agent::test_helpers::PerryHermesConfig::for_test_echo()`, with a one-time `use` alias if desired to keep call sites short.

**`crates/hermes-agent/src/runtime_agent.rs:270`:**

- Unchanged. The `use super::*` plus the `#[cfg(test)]` impl block already gives the test access to `for_test_echo`.

### Part 3 — kept as-is

- `AIAgent::for_test(agent_loop)` (`runtime_agent.rs:180-185`): private, 1 caller, no duplication. Renaming would add churn for no win.
- `parse_sse_for_test(input)` (`hermes-providers/src/openai/sse.rs:188`): single call site, already `pub(crate)`. No duplication to remove.

## Affected Files

- `crates/hermes-cli/src/tui/app.rs` — refactor `App` constructors
- `crates/hermes-cli/src/tui/run.rs` — use `App::new(...)` in `run` and `run_with_backend`
- `crates/hermes-cli/src/tui/{app,input,render}.rs` — `new_for_test` → `default`
- `crates/hermes-cli/tests/tui_*.rs` — `new_for_test` → `default`
- `crates/hermes-agent/src/config.rs` — delete two pure-default helpers, change `cfg` gate
- `crates/hermes-agent/tests/common/mod.rs` — delete
- `crates/hermes-agent/tests/context_compression.rs` — update import
- `crates/hermes-agent/tests/skills_injection.rs` — update import

## Acceptance Criteria

1. `cargo test --workspace` passes (all unit + integration tests).
2. `cargo clippy --workspace --all-targets` reports no new warnings.
3. Net LOC reduction ≥ 65 lines across the changed files.
4. No release binary references `test_helpers` — the `#[cfg(test)] pub mod test_helpers` gate keeps it out of `cargo build --release` and `cargo publish`.
5. The `App` struct still has all 20 public fields exposed (no behavior change for direct field access in tests).

## Out of Scope

- A `TestFixture` builder pattern for App (e.g. `App::with_input(...).with_cursor(3).build()`). Would compress the `let mut app = ...; app.X = ...; app.Y = ...;` chains in tests but is a much larger refactor with a different risk profile.
- Renaming `AIAgent::for_test(agent_loop)` to a more accurate name.
- Converting `parse_sse_for_test` into a `#[cfg(test)] mod tests` block.

## Risk

- No `cfg` gate change is needed. The module stays `#[cfg(test)] pub mod test_helpers`. The only thing that goes away is the duplicate `tests/common/mod.rs` — both code paths are exposed by `cargo test`'s `cfg(test)` setting, so no new build configuration gains visibility.
- The `App::new` constructor changes the call signature in two production sites; both are internal to `hermes-cli` and both will be updated in the same change.
- Integration tests changing their import path is a 1-line edit per test file; trivial blast radius.
