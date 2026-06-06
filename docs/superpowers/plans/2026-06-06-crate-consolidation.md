# Crate Consolidation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Consolidate the workspace from seven crates to five by introducing `hermes-agent` and merging `hermes-loop`, `hermes-runtime`, and `hermes-tools` into it while preserving the useful boundaries around `hermes-core`, `hermes-providers`, `hermes-skill-loader`, and `hermes-cli`.

> **Note (Phase 10):** `hermes-skills` was renamed to `hermes-skill-loader` to make its data-only scope explicit. See `2026-06-06-phase-10-rename-and-tui-design.md`.

**Architecture:** Build the new `hermes-agent` crate in phases. First move the current runtime assembly and built-in tools into the new crate, then move the loop engine into the same crate, and finally remove the old crate entries and refresh all references. Keep one responsibility in one internal module; do not let the consolidation recreate fragmentation inside the new crate.

**Tech Stack:** Rust workspace, Cargo, `tokio`, `reqwest`, `httpmock`, `tempfile`

---

### Task 1: Create `hermes-agent` and migrate runtime assembly plus built-in tools

**Files:**
- Create: `crates/hermes-agent/Cargo.toml`
- Create: `crates/hermes-agent/src/lib.rs`
- Create: `crates/hermes-agent/src/runtime_agent.rs`
- Create: `crates/hermes-agent/src/config.rs`
- Create: `crates/hermes-agent/src/prompting.rs`
- Create: `crates/hermes-agent/src/provider_factory.rs`
- Create: `crates/hermes-agent/src/tool_catalog.rs`
- Create: `crates/hermes-agent/src/tools/mod.rs`
- Create: `crates/hermes-agent/src/tools/bash.rs`
- Create: `crates/hermes-agent/tests/skills_injection.rs`
- Create: `crates/hermes-agent/tests/bash.rs`
- Create: `crates/hermes-agent/examples/live_tool_use.rs`
- Modify: `Cargo.toml`
- Modify: `crates/hermes-cli/Cargo.toml`

- [ ] **Step 1: Write the failing crate-presence test**

Create `crates/hermes-agent/tests/skills_injection.rs` by copying the existing `crates/hermes-runtime/tests/skills_injection.rs`, but change the import line to:

```rust
use hermes_agent::{AIAgent, HermesConfig, ProviderConfig, ProviderKind, SessionContext};
```

This should be the first failing test for the new crate surface.

- [ ] **Step 2: Run the targeted test to verify it fails**

Run: `cargo test -p hermes-agent runtime_new_preserves_user_prompt_without_skills_dir -- --exact`

Expected: FAIL because the `hermes-agent` crate does not exist yet.

- [ ] **Step 3: Create the new crate manifest and runtime modules**

Create `crates/hermes-agent/Cargo.toml` using the current `hermes-runtime` manifest as the base, but:

- rename the package to `hermes-agent`
- replace the old `hermes-tools` dependency by embedding the tool module locally
- keep dependencies on `hermes-core`, `hermes-providers`, and `hermes-skill-loader`

Create `crates/hermes-agent/src/lib.rs`:

```rust
//! Runtime engine shared by CLI and future gateways.

mod config;
mod prompting;
mod provider_factory;
mod runtime_agent;
mod tool_catalog;
pub mod tools;

pub use config::{AgentConfig, HermesConfig, ProviderConfig, ProviderKind};
pub use hermes_loop::{AgentLoop, LoopConfig, LoopEvent, RunResult};
pub use prompting::DEFAULT_SYSTEM_PROMPT;
pub use runtime_agent::{AIAgent, SessionContext};
```

Then copy the current `crates/hermes-runtime/src/agent.rs` into `crates/hermes-agent/src/runtime_agent.rs`, but adjust imports so it uses:

```rust
use crate::config::HermesConfig;
use crate::provider_factory::build_provider;
use crate::prompting::compose_system_prompt;
use crate::tool_catalog::build_registry;
```

Copy these files verbatim with import-path fixes:

- `crates/hermes-runtime/src/config.rs` -> `crates/hermes-agent/src/config.rs`
- `crates/hermes-runtime/src/prompting.rs` -> `crates/hermes-agent/src/prompting.rs`
- `crates/hermes-runtime/src/provider_factory.rs` -> `crates/hermes-agent/src/provider_factory.rs`
- `crates/hermes-runtime/src/tool_catalog.rs` -> `crates/hermes-agent/src/tool_catalog.rs`

Move the built-in tool into the new crate by copying:

- `crates/hermes-tools/src/bash.rs` -> `crates/hermes-agent/src/tools/bash.rs`
- create `crates/hermes-agent/src/tools/mod.rs` with:

```rust
pub mod bash;

pub use bash::BashTool;
```

Update `tool_catalog.rs` to import:

```rust
use crate::tools::BashTool;
```

- [ ] **Step 4: Add the copied tests and example to the new crate**

Copy:

- `crates/hermes-runtime/tests/skills_injection.rs` -> `crates/hermes-agent/tests/skills_injection.rs`
- `crates/hermes-tools/tests/bash.rs` -> `crates/hermes-agent/tests/bash.rs`
- `crates/hermes-runtime/examples/live_tool_use.rs` -> `crates/hermes-agent/examples/live_tool_use.rs`

Then adjust imports:

```rust
use hermes_agent::...
```

and for the tool test:

```rust
use hermes_agent::tools::BashTool;
```

- [ ] **Step 5: Wire the workspace to recognize `hermes-agent`**

Edit the workspace root `Cargo.toml`:

- add `"crates/hermes-agent"` to `members`
- add:

```toml
hermes-agent = { path = "crates/hermes-agent" }
```

to `[workspace.dependencies]`

Edit `crates/hermes-cli/Cargo.toml` to replace:

```toml
hermes-runtime.workspace = true
```

with:

```toml
hermes-agent.workspace = true
```

- [ ] **Step 6: Run focused verification for the new crate**

Run: `cargo test -p hermes-agent runtime_new_preserves_user_prompt_without_skills_dir -- --exact`

Expected: PASS

Run: `cargo test -p hermes-agent --test bash`

Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/hermes-cli/Cargo.toml crates/hermes-agent
git commit -m "feat(agent): create consolidated runtime and tool crate"
```

### Task 2: Move the loop engine into `hermes-agent`

**Files:**
- Create: `crates/hermes-agent/src/loop_engine.rs`
- Create: `crates/hermes-agent/tests/arg_validation.rs`
- Create: `crates/hermes-agent/tests/echo_loop.rs`
- Create: `crates/hermes-agent/tests/tool_dispatch.rs`
- Create: `crates/hermes-agent/tests/usage_metrics.rs`
- Create: `crates/hermes-agent/tests/support/mod.rs`
- Modify: `crates/hermes-agent/src/lib.rs`
- Modify: `crates/hermes-agent/src/runtime_agent.rs`
- Modify: `crates/hermes-agent/Cargo.toml`

- [ ] **Step 1: Write the failing loop-consolidation test**

Create `crates/hermes-agent/tests/echo_loop.rs` by copying `crates/hermes-loop/tests/echo_loop.rs` and change imports to:

```rust
use hermes_agent::{AgentLoop, LoopConfig};
```

- [ ] **Step 2: Run the targeted test to verify it fails**

Run: `cargo test -p hermes-agent --test echo_loop`

Expected: FAIL because `AgentLoop` and related loop types are not yet exported from `hermes-agent`.

- [ ] **Step 3: Move the loop engine source**

Copy:

- `crates/hermes-loop/src/agent.rs` -> `crates/hermes-agent/src/loop_engine.rs`

Then update `crates/hermes-agent/src/lib.rs`:

```rust
mod loop_engine;
```

and export:

```rust
pub use loop_engine::{AgentLoop, LoopConfig, LoopEvent, RunResult};
```

Update `crates/hermes-agent/src/runtime_agent.rs` to import loop types from `crate`:

```rust
use crate::{AgentLoop, LoopConfig, LoopEvent, RunResult};
```

Remove any remaining `use hermes_loop::...` imports in `hermes-agent`.

- [ ] **Step 4: Move loop integration tests**

Copy the current loop tests:

- `crates/hermes-loop/tests/arg_validation.rs` -> `crates/hermes-agent/tests/arg_validation.rs`
- `crates/hermes-loop/tests/echo_loop.rs` -> `crates/hermes-agent/tests/echo_loop.rs`
- `crates/hermes-loop/tests/tool_dispatch.rs` -> `crates/hermes-agent/tests/tool_dispatch.rs`
- `crates/hermes-loop/tests/usage_metrics.rs` -> `crates/hermes-agent/tests/usage_metrics.rs`
- `crates/hermes-loop/tests/support/mod.rs` -> `crates/hermes-agent/tests/support/mod.rs`

Adjust imports from `hermes_loop` and `hermes_tools` to `hermes_agent`, preferring:

```rust
use hermes_agent::{AgentLoop, LoopConfig, ...};
use hermes_agent::tools::BashTool;
```

- [ ] **Step 5: Update the new crate manifest**

Edit `crates/hermes-agent/Cargo.toml` to include the dependencies that were formerly in `hermes-loop`:

- `jsonschema`
- `thiserror`
- `futures`
- `async-trait`

and add any dev-dependencies needed by the copied loop tests.

- [ ] **Step 6: Run focused verification**

Run: `cargo test -p hermes-agent --test echo_loop`

Expected: PASS

Run: `cargo test -p hermes-agent`

Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add crates/hermes-agent
git commit -m "refactor(agent): absorb loop engine and tests"
```

### Task 3: Switch remaining consumers and examples to `hermes-agent`

**Files:**
- Modify: `crates/hermes-cli/src/main.rs`
- Modify: `crates/hermes-cli/tests/cli_smoke.rs`
- Modify: `README.md`
- Modify: `Cargo.toml`

- [ ] **Step 1: Write the failing CLI import test**

Add or update one import-sensitive CLI smoke test so it builds only through `hermes-agent`. If needed, use the existing `crates/hermes-cli/tests/cli_smoke.rs` file as the guardrail and rely on build failure as the red phase.

- [ ] **Step 2: Run a focused CLI test to verify the break**

Run: `cargo test -p hermes-cli hermes_picks_up_cwd_hermes_toml -- --exact`

Expected: FAIL until `hermes-cli` imports are switched from `hermes-runtime` to `hermes-agent`.

- [ ] **Step 3: Update CLI imports**

Edit `crates/hermes-cli/src/main.rs` and replace imports from `hermes_runtime` with:

```rust
use hermes_agent::{AIAgent, HermesConfig, LoopEvent, SessionContext};
```

Adjust any example or test imports the same way.

- [ ] **Step 4: Refresh docs and commands**

Update `README.md` to reflect:

- the workspace now has five crates
- `hermes-agent` is the runtime engine crate
- example commands use:

```bash
cargo run -p hermes-agent --example live_tool_use -- "what time is it?"
```

- [ ] **Step 5: Run CLI verification**

Run: `cargo test -p hermes-cli`

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-cli README.md Cargo.toml
git commit -m "refactor(cli): switch to hermes-agent crate"
```

### Task 4: Remove legacy crates from the workspace

**Files:**
- Modify: `Cargo.toml`
- Delete: `crates/hermes-loop/Cargo.toml`
- Delete: `crates/hermes-loop/src/lib.rs`
- Delete: `crates/hermes-loop/src/agent.rs`
- Delete: `crates/hermes-loop/tests/arg_validation.rs`
- Delete: `crates/hermes-loop/tests/echo_loop.rs`
- Delete: `crates/hermes-loop/tests/support/mod.rs`
- Delete: `crates/hermes-loop/tests/tool_dispatch.rs`
- Delete: `crates/hermes-loop/tests/usage_metrics.rs`
- Delete: `crates/hermes-runtime/Cargo.toml`
- Delete: `crates/hermes-runtime/src/lib.rs`
- Delete: `crates/hermes-runtime/src/agent.rs`
- Delete: `crates/hermes-runtime/src/config.rs`
- Delete: `crates/hermes-runtime/src/prompting.rs`
- Delete: `crates/hermes-runtime/src/provider_factory.rs`
- Delete: `crates/hermes-runtime/src/tool_catalog.rs`
- Delete: `crates/hermes-runtime/tests/skills_injection.rs`
- Delete: `crates/hermes-runtime/examples/live_tool_use.rs`
- Delete: `crates/hermes-tools/Cargo.toml`
- Delete: `crates/hermes-tools/src/lib.rs`
- Delete: `crates/hermes-tools/src/bash.rs`
- Delete: `crates/hermes-tools/tests/bash.rs`

- [ ] **Step 1: Write the failing workspace-shape test**

Use the workspace build itself as the failing guardrail after member removal.

Run: `cargo test -p hermes-runtime`

Expected: PASS now, and it will become invalid once the crate is removed. This confirms the crate still exists before deletion.

- [ ] **Step 2: Remove the old workspace members**

Edit the root `Cargo.toml` to remove:

- `"crates/hermes-loop"`
- `"crates/hermes-runtime"`
- `"crates/hermes-tools"`

and remove their entries from `[workspace.dependencies]`.

- [ ] **Step 3: Delete the old crate files**

Delete the three legacy crate directories after confirming every reference has moved to `hermes-agent`.

- [ ] **Step 4: Run focused workspace verification**

Run: `cargo test -p hermes-agent`

Expected: PASS

Run: `cargo test -p hermes-cli`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/hermes-agent crates/hermes-cli README.md
git rm -r crates/hermes-loop crates/hermes-runtime crates/hermes-tools
git commit -m "refactor(workspace): consolidate runtime crates into hermes-agent"
```

### Task 5: Run final verification and refresh consolidation docs

**Files:**
- Modify: `docs/superpowers/specs/2026-06-06-crate-consolidation-design.md`
- Modify: `docs/superpowers/specs/2026-06-06-architecture-cohesion-refactor-design.md`

- [ ] **Step 1: Update docs to reflect the finished crate layout**

Adjust both specs so they describe the actual final state rather than the pre-migration target.

- [ ] **Step 2: Run full workspace verification**

Run: `cargo test`

Expected: PASS

- [ ] **Step 3: Run one final focused engine verification**

Run: `cargo test -p hermes-agent`

Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/2026-06-06-crate-consolidation-design.md docs/superpowers/specs/2026-06-06-architecture-cohesion-refactor-design.md
git commit -m "docs: refresh consolidated crate architecture"
```
