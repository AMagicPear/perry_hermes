# Architecture Cohesion Refactor Design

**Date:** 2026-06-06
**Project:** `perry_hermes`
**Goal:** Improve overall code structure toward higher cohesion and lower coupling by tightening crate boundaries, shrinking the runtime composition surface, and removing boundary leaks from the core layer.

## Context

The workspace already has a strong top-level split:

- `hermes-core` for shared domain types and traits
- `hermes-providers` for model adapters
- `hermes-tools` for built-in tools
- `hermes-loop` for the agent loop
- `hermes-runtime` for assembly
- `hermes-cli` for the REPL surface

The current issue is not missing layers, but boundary drift inside those layers. Several responsibilities that should stay local to one crate have started to bleed into others, especially around transport errors, runtime wiring, and tool/provider construction.

## Current Structural Problems

### 1. Core layer knows transport details

This was addressed by changing `ProviderError::Transport` to a transport-agnostic `String` payload and removing `reqwest` from `hermes-core`. The previous direct `reqwest::Error` dependency broke the stated contract that `hermes-core` is the IO-free foundation and created an upward dependency leak from providers into the core abstraction layer.

### 2. Runtime is carrying too many responsibilities

Before the refactor, `hermes-runtime` did all of the following:

- provider construction
- provider-specific config interpretation
- skills directory discovery
- system prompt composition
- tool registry construction
- toolset enable/disable policy
- user-facing `AIAgent` assembly

These pieces all relate to startup, but they do not share the same reason to change. That makes the crate less cohesive and makes future additions cluster into one file.

### 3. Tool registration and tool policy are hard-wired together

The runtime currently decides both what tools exist and which toolsets are enabled. That makes adding a new tool require changes in the same location that owns policy and assembly, which increases coupling.

### 4. Provider-specific config translation lives in generic runtime code

Anthropic thinking mode translation was implemented in runtime assembly code instead of next to provider construction logic. This pushed provider-specific knowledge into a crate that should remain generic.

## Refactor Objectives

1. Keep `hermes-core` pure from transport and provider implementation details.
2. Make `hermes-runtime` a thinner composition layer with small focused internal modules.
3. Move provider-specific construction logic behind a dedicated builder boundary.
4. Move tool catalog construction behind a dedicated registry/catalog boundary.
5. Preserve external behavior of the CLI and runtime APIs unless a change is required for cleaner boundaries.

## Chosen Approach

Use a boundary-tightening refactor without adding new top-level crates.

This keeps the existing workspace understandable while fixing the actual coupling problems. We will prefer small internal modules inside `hermes-runtime` over introducing more crates prematurely.

## Target Architecture

### `hermes-core`

Responsibilities:

- shared domain messages and usage types
- provider and tool traits
- loop-facing error enums
- registry abstractions

Constraint after refactor:

- no direct dependency on `reqwest`
- provider errors represented in transport-agnostic form

### `hermes-providers`

Responsibilities:

- HTTP clients and stream parsing
- provider request/response translation
- provider-specific config interpretation needed to instantiate providers
- mapping external failures into `hermes_core::ProviderError`

After refactor:

- expose a small provider factory surface used by runtime
- own conversion from provider config into provider instances

Current implementation note:

- the factory currently lives in `hermes-runtime::provider_factory`, which already centralizes provider-specific construction and keeps it out of `agent` execution logic
- a future move into `hermes-providers` is still possible, but only if done without introducing config-model scattering or circular dependencies

### `hermes-tools`

Responsibilities:

- concrete tool implementations
- tool catalog helpers for built-in tools

After refactor:

- runtime asks for a registry/catalog from a focused helper
- tool existence and toolset filtering stop being inlined in runtime assembly code

### `hermes-runtime`

Responsibilities:

- load runtime config
- compose system prompt from base prompt and loaded skills
- assemble provider + tools + loop into `AIAgent`

After refactor:

- split into focused modules such as `agent`, `prompting`, `provider_factory`, and `tool_catalog`
- keep `lib.rs` as a narrow public surface, not the implementation dump site

### `hermes-cli`

Responsibilities:

- argument parsing
- config path resolution
- interactive session UX

No structural expansion is planned here.

## Concrete Changes

### Change Set A: remove transport coupling from `hermes-core`

- Change `ProviderError::Transport` to hold a transport-agnostic payload such as `String`
- Remove `reqwest` from `hermes-core/Cargo.toml`
- Update provider implementations to convert `reqwest::Error` into the new error form at the boundary
- Update tests that currently pattern-match transport errors
- Remove duplicate parser-level transport tests once a higher-signal provider boundary test exists

### Change Set B: split runtime assembly into cohesive modules

Inside `crates/hermes-runtime/src/` introduce focused files:

- `agent.rs` for `AIAgent` and `SessionContext`
- `prompting.rs` for default prompt + skills prompt composition
- `tool_catalog.rs` for built-in registry construction
- keep provider construction in either a runtime `provider_factory.rs` or preferably inside `hermes-providers`

The goal is to make each file answer one question clearly instead of one file answering every startup question.

### Change Set C: move provider construction out of generic runtime flow

Chosen direction for this iteration:

- add a dedicated `provider_factory.rs` module inside `hermes-runtime`
- keep all provider-specific construction rules in that one module
- avoid moving config types or introducing a cycle just to satisfy crate purity cosmetically

This still keeps provider-specific thinking logic out of `agent` execution code while preserving cohesion.

### Change Set D: move built-in registry creation behind a dedicated helper

- create a small built-in tool registry constructor
- centralize toolset filtering there
- keep runtime assembly code unaware of individual tool types except through that helper

## Non-Goals

- no new top-level crates unless a boundary cannot be made clean internally
- no behavioral redesign of the agent loop
- no feature expansion for new tools or providers
- no CLI UX redesign

## Risks and Mitigations

### Risk: public API churn

Mitigation:

- preserve `AIAgent::from_config`, `AIAgent::new`, `run_turn`, and `run_messages`
- keep re-exports stable where practical

### Risk: tests become brittle during moves

Mitigation:

- add focused regression tests before changing boundaries
- move code in small slices
- verify crate-level tests after each structural step

### Risk: over-refactoring

Mitigation:

- stop at boundary cleanup
- do not introduce extra abstraction layers unless they remove a real dependency

## Testing Strategy

This refactor will follow TDD for each behavior-affecting or boundary-affecting change:

1. add or adjust tests that capture the intended boundary
2. run the targeted failing test
3. implement the minimum structural change
4. rerun targeted tests
5. rerun broader workspace verification

Expected verification commands:

- `cargo test -p hermes-core`
- `cargo test -p hermes-providers`
- `cargo test -p hermes-runtime`
- `cargo test`

## Execution Order

1. Remove the `reqwest` leak from `hermes-core`
2. Extract runtime internals into smaller focused modules without changing behavior
3. Move provider factory logic behind a narrower boundary
4. Extract built-in tool registry construction
5. Run full test verification and review resulting boundaries

## Success Criteria

- `hermes-core` no longer depends on `reqwest`
- `hermes-runtime/src/lib.rs` becomes a thin public entry point
- provider-specific construction details are no longer mixed into generic runtime execution code
- built-in tool registration is isolated from `AIAgent` assembly
- duplicate transport tests are reduced in favor of higher-signal boundary tests
- all existing tests continue to pass
