# Crate Consolidation Design

**Date:** 2026-06-06
**Project:** `perry_hermes`
**Goal:** Consolidate the current workspace from seven crates to five crates so the agent runtime becomes more cohesive, while preserving the boundaries that still carry real value.

## Decision

Adopt a five-crate target layout:

- `hermes-core`
- `hermes-providers`
- `hermes-agent`
- `hermes-skills`
- `hermes-cli`

This means:

- merge `hermes-loop`, `hermes-runtime`, and `hermes-tools` into a new `hermes-agent` crate
- keep `hermes-core`, `hermes-providers`, `hermes-skills`, and `hermes-cli` as separate crates

## Why This Consolidation Is Worth Doing

The current seven-crate layout has already crossed the point where some of the split boundaries are costing more than they are buying back.

The strongest signs are:

- `hermes-loop` and `hermes-runtime` change together frequently
- `hermes-tools` is still effectively a single-tool crate
- runtime assembly, loop behavior, and built-in tools are all part of one execution pipeline
- tests and file layout already treat these areas as tightly related

The result is a shape that looks modular from far away, but still requires frequent cross-crate jumps to understand or change one runtime behavior end to end.

## Why These Five Crates

### `hermes-core`

Keep it separate.

This crate still has a real architectural boundary:

- message model
- provider and tool traits
- shared error types
- registries
- usage and stream accumulation support

It is the lowest-level stable contract and is useful precisely because it does not know about HTTP, CLI, skills loading, or concrete tools.

### `hermes-providers`

Keep it separate.

This crate is the clean adapter boundary for external LLM APIs:

- OpenAI-compatible transport
- Anthropic transport
- wire-format parsing
- provider-specific request shaping

It depends on `hermes-core`, but its change rate and failure modes are distinct from the rest of the runtime.

### `hermes-agent`

Create this by merging the current runtime-facing execution crates.

This crate should own the entire in-process agent engine:

- `AgentLoop`
- `AIAgent`
- runtime config loading and interpretation
- prompt composition
- provider factory
- tool catalog
- built-in tools like `BashTool`
- loop events
- session context

These pieces are one cohesive runtime subsystem. They are not independent products.

### `hermes-skills`

Keep it separate.

This crate already has enough substance to justify independence:

- frontmatter parsing
- validation
- skill layout discovery
- system-prompt block rendering

It has different concerns from the agent loop itself, and it already benefits from being tested independently without dragging in the whole runtime.

### `hermes-cli`

Keep it separate.

The CLI is the product shell:

- clap parsing
- config path resolution
- REPL loop
- terminal UX
- Ctrl-C handling

This remains a clear outermost boundary and should stay thin.

## Why Not Four Crates

A four-crate version would likely merge `hermes-skills` into `hermes-agent`.

That is possible, but not recommended right now because:

- `hermes-skills` is already non-trivial in size
- it has its own test surface
- its responsibility is conceptually separate from loop execution
- merging it now would make the new `hermes-agent` crate fatter than it needs to be

The five-crate target gets most of the cohesion win without collapsing distinct concerns unnecessarily.

## New Boundary Map

### Target dependency direction

```text
hermes-cli
  -> hermes-agent
      -> hermes-core
      -> hermes-providers
      -> hermes-skills

hermes-providers
  -> hermes-core

hermes-skills
  -> (no internal crate dependency required)
```

Key property:

- no runtime split between loop, tools, and facade
- no circular dependencies
- the CLI remains the only outer application shell

## What Moves Into `hermes-agent`

### From `hermes-loop`

- `src/agent.rs`
- `src/lib.rs`
- loop-specific tests under `crates/hermes-loop/tests/`

### From `hermes-runtime`

- `src/agent.rs`
- `src/config.rs`
- `src/prompting.rs`
- `src/provider_factory.rs`
- `src/tool_catalog.rs`
- `src/lib.rs`
- examples and tests that still make sense as runtime-engine tests

### From `hermes-tools`

- `src/bash.rs`
- `src/lib.rs`
- `tests/bash.rs`

## Proposed Internal Module Structure for `hermes-agent`

The goal is not to create one giant `lib.rs`. The goal is to keep one runtime crate with clear internal modules.

Suggested structure:

```text
crates/hermes-agent/src/
  lib.rs
  loop_engine.rs        # current hermes-loop agent state machine
  runtime_agent.rs      # current AIAgent facade and SessionContext
  config.rs             # HermesConfig / ProviderConfig / AgentConfig
  prompting.rs          # system prompt + skills injection
  provider_factory.rs   # provider construction
  tool_catalog.rs       # built-in registry assembly
  tools/
    mod.rs
    bash.rs
```

The important rule is this:

- one responsibility should live in one obvious module
- the consolidation must not replace too many crates with one sprawling file

## Public API Shape After Consolidation

The public API should stay close to what callers already know:

- `AIAgent`
- `SessionContext`
- `HermesConfig`
- `ProviderConfig`
- `ProviderKind`
- `LoopEvent`
- `AgentLoop`
- `LoopConfig`
- `RunResult`
- built-in tool exports only if there is a real external need

Where possible, existing import paths used by `hermes-cli` examples and tests should be preserved through re-exports during the migration.

## Test Consolidation Strategy

This migration should also simplify the test layout.

### Keep

- high-signal integration tests for loop behavior
- config wiring tests
- skills injection tests
- provider adapter tests
- CLI smoke tests
- one built-in tool behavior test per tool

### Merge or move

- `hermes-loop` tests move under `hermes-agent/tests/`
- `hermes-tools/tests/bash.rs` moves under `hermes-agent/tests/`
- runtime tests currently in `hermes-runtime/src/agent.rs` and `tests/skills_injection.rs` stay, but under the consolidated crate

### Avoid

- duplicating the same boundary test at parser, assembly, and integration layers
- keeping crate-specific test separation when the crates no longer exist

## Migration Order

Do this in phases to keep the change reviewable.

### Phase 1: Create `hermes-agent` and move runtime assembly + tools first

Scope:

- create new `crates/hermes-agent`
- move `hermes-runtime` modules into it
- move `hermes-tools` into it
- keep `hermes-loop` temporarily separate

Why first:

- `hermes-tools` is the thinnest crate and easiest win
- runtime already owns tool catalog and provider factory decisions
- this reduces one boundary quickly without destabilizing the loop state machine yet

### Phase 2: Move `hermes-loop` into `hermes-agent`

Scope:

- migrate `AgentLoop`, `LoopConfig`, `RunResult`, `LoopEvent`
- move loop tests
- re-export the loop types from `hermes-agent`

Why second:

- loop behavior is the most central and the most likely to create compile fallout
- moving it after runtime/tools keeps the migration smaller per step

### Phase 3: Remove old crates and clean workspace references

Scope:

- remove `hermes-loop` from workspace members
- remove `hermes-runtime` from workspace members
- remove `hermes-tools` from workspace members
- update `Cargo.toml` dependencies across remaining crates
- update examples, README, docs, and test commands

## Cargo and Workspace Changes

The final workspace should look roughly like:

```toml
[workspace]
members = [
    "crates/hermes-core",
    "crates/hermes-providers",
    "crates/hermes-agent",
    "crates/hermes-skills",
    "crates/hermes-cli",
]
```

Likely dependency shifts:

- `hermes-cli` should depend on `hermes-agent` instead of `hermes-runtime`
- `hermes-agent` should depend on `hermes-core`, `hermes-providers`, and `hermes-skills`
- `hermes-providers` continues to depend only on `hermes-core`

## Risks

### Risk 1: one large crate replaces several small ones but loses clarity

Mitigation:

- keep a disciplined internal module layout
- do not centralize everything into one `lib.rs`
- keep execution, prompting, config, tool catalog, and concrete tools in separate internal modules

### Risk 2: public import churn

Mitigation:

- introduce re-exports early
- update the CLI and examples first
- remove old paths only after all internal callers are moved

### Risk 3: tests become noisier during the transition

Mitigation:

- move tests with their responsibility
- prune duplicate tests while migrating
- run focused crate tests after each move, then `cargo test`

### Risk 4: provider construction logic becomes more scattered, not less

Mitigation:

- keep all provider construction in one module
- do not split provider factory logic between config parsing and agent assembly

## Success Criteria

- workspace reduced from seven crates to five crates
- `hermes-loop`, `hermes-runtime`, and `hermes-tools` no longer exist as separate crates
- runtime engine responsibilities are concentrated inside `hermes-agent`
- `hermes-core`, `hermes-providers`, `hermes-skills`, and `hermes-cli` remain cleanly bounded
- test layout is simpler than before, not merely relocated
- `cargo test` remains green throughout the migration

## Recommendation

Proceed with the five-crate consolidation, but do it in phased moves rather than a single giant rename.

This is one of those cases where fewer crates will genuinely improve cohesion instead of just flattening architecture. The key is to merge the runtime pipeline into one crate without recreating the same fragmentation internally.
