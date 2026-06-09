# Test Fixture Cleanup Design

**Date:** 2026-06-09
**Project:** `perry_hermes`
**Goal:** Reduce scattered, duplicated test-fixture construction code across the workspace, eliminate code smells where production code uses test-named constructors, delete every helper whose only value is a thin alias, and lower total LOC without losing functionality.

## Context

The workspace has accumulated three distinct "test fixture" patterns that have drifted into code smell territory:

1. `App::new_for_test()` in `hermes-cli` is called 88 times across unit tests, integration tests, **and two production `run()` entry points**. The name claims "for test" but the function is the de-facto App constructor. There is no `Default` impl, so every caller mechanically writes `let mut app = App::new_for_test();` and then mutates individual fields.

2. `for_test_echo` (and friends) for `PerryHermesConfig` and `ProviderConfig` is implemented **twice** — once in `crates/hermes-agent/src/config.rs` under `#[cfg(test)] pub mod test_helpers`, and once in `crates/hermes-agent/tests/common/mod.rs`. The duplication is rationalized by a comment that claims integration tests cannot see `#[cfg(test)]` items, which is incorrect: `cargo test` enables `cfg(test)` for both library and integration test crates, so integration tests **can** see the library's `#[cfg(test)] pub mod`. The duplication is historical baggage.

3. A pair of nested test helpers in `hermes-providers/src/openai/sse.rs`: `parse_sse_for_test` (a 15-line `pub(crate)` function that drives the real `parse_sse_chunks` over a single chunk) is wrapped by a 3-line private alias `parse_sse_bytes` that does nothing but call the wrapper. Two layers, one job.

Plus three dead-or-pointless helpers: `AIAgent::for_test` (private, 1 caller, 8-line block), `PerryHermesConfig::for_test_with` (zero callers — dead code), and the pure-default aliases `for_test_default` / `for_test_empty`.

**Deletion philosophy:** "If a helper has no callers, delete it. If a helper has one caller and the call site would be clearer without it, delete it. Don't preserve test plumbing out of deference to the original author."

## Current State

| Pattern | Location | Count | Action |
| --- | --- | --- | --- |
| `App::new_for_test()` call sites | `hermes-cli` | 88 | Rename to `App::default()` |
| `App::new_for_test()` definition | `hermes-cli/src/tui/app.rs:65-87` | 1 | Replace with `impl Default for App` |
| `run()` / `run_with_backend` 5-line "build then mutate" | `hermes-cli/src/tui/run.rs:78-82, 183-187` | 2 | Collapse to `App::new(provider, model, max_iter, ctx_window)` |
| `PerryHermesConfig::for_test_echo` | `hermes-agent/src/config.rs:326-336` | 1 | Keep — real construction logic |
| `ProviderConfig::for_test_echo` | `hermes-agent/src/config.rs:365-378` | 1 | Keep — real construction logic |
| `PerryHermesConfig::for_test_empty` | `hermes-agent/src/config.rs:339-341` | 1 | **Delete** — pure `Self::default()` |
| `PerryHermesConfig::for_test_with` | `hermes-agent/src/config.rs:345-351` | 0 callers | **Delete** — dead code |
| `AgentConfig::for_test_default` | `hermes-agent/src/config.rs:357-359` | 1 | **Delete** — pure `Self::default()` |
| `for_test_echo` (integration duplicate) | `hermes-agent/tests/common/mod.rs:17-27` | 1 | **Delete the whole file** |
| `for_test_provider_echo` (integration duplicate) | `hermes-agent/tests/common/mod.rs:29-42` | 1 | **Delete the whole file** |
| `AIAgent::for_test(agent_loop)` | `hermes-agent/src/runtime_agent.rs:178-186` | 1 | **Replace** with a 3-line public `AIAgent::new(agent_loop)` |
| `parse_sse_for_test` + `parse_sse_bytes` (two-layer) | `hermes-providers/src/openai/sse.rs:188-202, 212-214` | 8 test call sites of inner | **Collapse** to a single helper |
| `test_helpers` module leading comment | `hermes-agent/src/config.rs:314-320` | 1 | Update — current comment contains a false claim about `cfg(test)` visibility |

## Design

### Part 1 — `hermes-cli`: `App::default()` + `App::new(...)`, drop `new_for_test`

**`crates/hermes-cli/src/tui/app.rs`:**

- Remove `pub fn new_for_test() -> Self` (lines 65-87, 22 lines).
- Add `impl Default for App` whose body is the same field-init block (~13 lines including `impl`/`fn`/closing brace). Every test call site changes from `App::new_for_test()` to `App::default()`.
- Add `pub fn new(provider_name: String, model_name: String, max_iterations: u32, context_window_size: Option<u64>) -> Self` (~7 lines) that takes the four fields `run()` and `run_with_backend` currently set by hand.

**`crates/hermes-cli/src/tui/run.rs`:**

- In `pub async fn run(...)` (around line 78-82), replace the 5-line "build then mutate" block with one line:
  ```rust
  let mut app = App::new(provider_name, model_name, max_iterations, context_window_size);
  ```
- In `pub async fn run_with_backend(...)` (around line 183-187), do the same.

**Test files (`hermes-cli/src/tui/{app,input,render}.rs` and `hermes-cli/tests/tui_*.rs`):**

- Mechanical replacement: `App::new_for_test()` → `App::default()`. Field-assignment lines that follow each `let mut app = ...` stay as-is — collapsing them into a builder is out of scope.

### Part 2 — `hermes-agent`: de-duplicate `for_test_echo`, delete every helper with no live caller

**`crates/hermes-agent/src/config.rs`:**

- Keep the existing `#[cfg(test)] pub mod test_helpers` gate. `cargo test` sets `cfg(test)` for both the library and integration test binaries, so integration tests **already** have access to the library's `#[cfg(test)] pub mod`. The existing `tests/common/mod.rs` is redundant — it is not a workaround for a Rust limitation.
- Update the leading `test_helpers` module comment (currently lines 314-320) to remove the false claim that integration tests cannot see `cfg(test)` items, and to point integration tests at `perry_hermes_agent::test_helpers::*` directly.
- **Delete** `PerryHermesConfig::for_test_empty()` (lines 339-341): pure `Self::default()`, no value.
- **Delete** `PerryHermesConfig::for_test_with()` (lines 345-351): **zero callers** in the workspace, dead code.
- **Delete** `AgentConfig::for_test_default()` (lines 357-359): pure `Self::default()`, no value.
- Keep `PerryHermesConfig::for_test_echo()` and `ProviderConfig::for_test_echo()` — each contains real field construction logic used by tests.

**`crates/hermes-agent/tests/common/mod.rs`:**

- Delete the entire file (42 lines). The two integration test consumers (`tests/context_compression.rs`, `tests/skills_injection.rs`) replace `common::for_test_echo()` with `perry_hermes_agent::test_helpers::PerryHermesConfig::for_test_echo()` (or alias it via `use`).

**`crates/hermes-agent/src/runtime_agent.rs`:**

- **Delete** the `#[cfg(test)] impl AIAgent` block at lines 178-186 (8 lines, one private helper `for_test`).
- Add `pub fn new(agent_loop: AgentLoop) -> Self` to the main `impl AIAgent` block (~3 lines). This is a real naked constructor — useful for tests and any future low-level wiring — and the single test caller (line 591) updates from `AIAgent::for_test(agent_loop)` to `AIAgent::new(agent_loop)`. Net: -5 lines and a saner public surface.

### Part 3 — `hermes-providers`: collapse the two-layer SSE test helper

**`crates/hermes-providers/src/openai/sse.rs`:**

- **Delete** `parse_sse_for_test` (lines 187-202, 15 lines, `pub(crate)`).
- **Keep and rewrite** `parse_sse_bytes` (lines 212-214) as a single helper that contains the body of the old `parse_sse_for_test`. Rename to `parse_sse_chunk` (more accurate — it drives the parser over a single byte chunk and collects the deltas). The 8 test call sites of `parse_sse_bytes` become call sites of `parse_sse_chunk`.
- Net: -2 lines (one wrapper deleted, the other absorbed) and one fewer layer of indirection.

## Affected Files

- `crates/hermes-cli/src/tui/app.rs` — refactor `App` constructors
- `crates/hermes-cli/src/tui/run.rs` — use `App::new(...)` in `run` and `run_with_backend`
- `crates/hermes-cli/src/tui/{app,input,render}.rs` — `new_for_test` → `default`
- `crates/hermes-cli/tests/tui_*.rs` — `new_for_test` → `default`
- `crates/hermes-agent/src/config.rs` — delete three helpers, update `test_helpers` comment
- `crates/hermes-agent/src/runtime_agent.rs` — replace `for_test` block with `new(agent_loop)`
- `crates/hermes-agent/tests/common/mod.rs` — delete
- `crates/hermes-agent/tests/context_compression.rs` — update import
- `crates/hermes-agent/tests/skills_injection.rs` — update import
- `crates/hermes-providers/src/openai/sse.rs` — collapse two helpers into one

## Acceptance Criteria

1. `cargo test --workspace` passes (all unit + integration tests).
2. `cargo clippy --workspace --all-targets` reports no new warnings.
3. Net LOC reduction ≥ 90 lines across the changed files.
4. No release binary references `test_helpers` — the `#[cfg(test)] pub mod test_helpers` gate keeps it out of `cargo build --release` and `cargo publish`.
5. The `App` struct still has all 20 public fields exposed (no behavior change for direct field access in tests).
6. After Part 3, the only SSE test helper is the single `parse_sse_chunk` (no nested wrappers).
7. After Part 2, there are no `for_test_with`, `for_test_default`, `for_test_empty`, `for_test_echo` (in `tests/common`), `AIAgent::for_test`, or `parse_sse_for_test` symbols in the workspace.

## Out of Scope

- A `TestFixture` builder pattern for App (e.g. `App::with_input(...).with_cursor(3).build()`). Would compress the `let mut app = ...; app.X = ...; app.Y = ...;` chains in tests but is a much larger refactor with a different risk profile.
- Changing field visibility on `AIAgent` so test code can use struct literals — would widen the public API for a one-line saving.

## Risk

- The `cfg(test)` gate on `test_helpers` is unchanged. No new build configuration gains visibility into the module.
- The `App::new` constructor changes the call signature in two production sites; both are internal to `hermes-cli` and both will be updated in the same change.
- Adding `AIAgent::new(agent_loop)` is a small public API addition. The doc comment will direct production code to prefer `AIAgent::from_config`.
- Integration tests changing their import path is a 1-line edit per test file; trivial blast radius.
- Collapsing the SSE helpers changes the name of a `pub(crate)` function (private visibility hides the rename from downstream crates).
