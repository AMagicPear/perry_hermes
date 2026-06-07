# Dead Code and Redundancy Cleanup Design

**Date:** 2026-06-07
**Project:** `perry_hermes`
**Goal:** Reduce dead code, orphan pub items, redundant logic, and unused dependencies across the workspace through five focused passes that each ship a clean, verified commit. The depth-of-understanding principle: every deletion must be backed by reading the surrounding code, not by trusting a tool's verdict.

## Decision

Adopt a **five-pass cleanup** structured around different categories of redundancy, run sequentially. Each pass:

1. **Discovers** candidates via a specific tool or audit method.
2. **Reads** the surrounding code for every candidate before deletion, to confirm intent and rule out false positives.
3. **Deletes** in small, atomic units.
4. **Verifies** with `cargo test --workspace` and `cargo clippy --all-targets --all-features -- -D warnings`.

The five passes are:

1. **Pass 1 — Unused dependencies** (Cargo.toml surface)
2. **Pass 2 — Compiler-detected dead code** (non-pub `unused_*`, `dead_code`, `unreachable_code` warnings)
3. **Pass 3 — Workspace-grep pub surface audit** (orphan `pub` items; this is the highest-leverage pass and the one the user explicitly called out as needing depth)
4. **Pass 4 — Redundant logic** (clippy `redundant_*`, `or_fun_call`, `single_match`, etc.)
5. **Pass 5 — tests/ and examples/ helpers** (unreferenced fixtures, duplicate setup, dead examples)

Each pass produces one or more commits on `main`. The branch is never force-pushed.

## Why This Is Worth Doing

The codebase is at a size where dead code is a real maintenance tax:

- `anthropic.rs` is 896 lines, `tui/render.rs` is 581 lines, `loop_engine.rs` is 563 lines.
- Recent refactors (`优化算法和代码结构`, `继续优化精简代码`, `清理少量无用代码`, `tighten runtime and tool architecture`) show the user has been doing this incrementally; an explicit pass consolidates the work.
- The project is "experimental and has no users" (CLAUDE.md) — we are free to shrink the public surface aggressively.

The cost of *not* doing it:

- Future readers (human and LLM) waste time on code that does nothing.
- Refactors become riskier because the blast radius includes dead paths.
- `cargo doc` output drifts from the real API.

## What This Is Not

- **Not** a behavior change. If a refactor is needed for clarity, that's a separate task.
- **Not** an architecture redesign. No new traits, no new modules, no crate boundary changes.
- **Not** a documentation pass. `///` doc updates only happen when removing the item they document.
- **Not** a test addition pass. We only delete tests/helpers, never add new ones (unless a removed test breaks coverage, in which case the test is rewritten, not added).
- **Not** touching the repo root binaries (`rust_out`, `.envrc`, etc.) or `docs/`.

## The Depth-of-Understanding Principle

For every candidate deletion in Passes 2–5, the executor MUST read the surrounding code (the function body, the call site if any, the type that owns the field, the trait that the impl satisfies) before deleting. Mechanical grep-and-delete is forbidden because:

- The compiler says "dead" for `pub` items only if no other crate in the workspace uses them — but `pub(crate)` items used by 0 things in the same crate are NOT flagged, and a `pub fn` with a default impl on a trait that's never constructed still looks live.
- clippy lint suggestions can be wrong: a `clone()` that looks redundant may be on a value whose `Drop` runs external resources.
- `cargo machete` has false positives for build-script-deps, proc-macro deps, and `#[cfg(...)]`-gated code.

**The rule:** for every deletion, write down (in the commit body or a scratch note) one sentence on what the code was doing and why it's safe to remove. If you cannot write that sentence, do not delete.

## Pass Details

### Pass 1 — Unused Dependencies

**Tooling:**
- `cargo install cargo-machete` (stable) for regular and dev-dependency scan
- `cargo +nightly udeps` if a nightly toolchain is installed; else manual audit of dev-dependencies

**Method:**
- For each `Cargo.toml` in the workspace, run the tools and capture the report.
- For each candidate, run a final `rg` across the workspace for the crate name in `use` statements, `[features]`, `[build-dependencies]`, and `[target.*]` sections. If truly zero hits, remove.
- Re-run `cargo build` and `cargo test` after each crate's `Cargo.toml` change.

**Risk:** low. Compile failures are obvious and revertible.

**Crate order:** leaf crates first (`hermes-core`, `hermes-providers`, `hermes-skill-loader`), then `hermes-agent`, then `hermes-cli`. This minimizes cross-crate compile errors.

### Pass 2 — Compiler-Detected Dead Code

**Tooling:**
- `cargo build --workspace --all-targets 2>&1 | tee build.log`
- `cargo test --workspace --no-run 2>&1 | tee test-build.log`
- `cargo doc --workspace --no-deps 2>&1 | tee doc.log`

**Categories to address:**
- `unused_imports`
- `unused_variables` (only when prefixed with `_` already — otherwise rename to `_x` if obvious, or delete)
- `dead_code` (functions, structs, enums, fields, methods)
- `unused_assignments`
- `unused_must_use`
- `unreachable_code`
- `unreachable_patterns`
- `unused_braces`, `unused_parens` (low priority, fix during clippy pass instead)

**Method:** for each warning, locate the code, read 30+ lines of context, confirm the item is truly orphaned (not referenced via reflection, trait object, `format!("{name}")`, or `serde_json` magic), then delete.

**Special case:** `dead_code` warnings on `pub` items are deferred to Pass 3 — the compiler doesn't see cross-crate uses, so we cannot trust its verdict on `pub` without the workspace grep.

### Pass 3 — Workspace-grep Pub Surface Audit

This is the highest-leverage pass and the one explicitly called out by the user as needing depth.

**Tooling:**
- `rg` for the workspace grep
- For each crate, generate a list of `pub` items with: `rg -n '^\s*pub\s+(fn|struct|enum|trait|const|static|type|use)\b' crates/<crate>/src/`

**Method, per item:**

1. Note the item's name and location.
2. `rg -n '\b<item_name>\b' crates/` — search the entire workspace, excluding the item's own definition line.
3. If 0 hits outside the defining crate, the item is orphan. **Stop and read the source before deleting.** Specifically:
   - Why was it `pub`? Look at git blame if it tells a story (e.g. "this used to be re-exported by `lib.rs`").
   - Is it part of a `pub trait` whose other methods ARE used? Trait methods must move together.
   - Is it referenced in `docs/`, `README.md`, or `examples/config/*.toml`? (Not in scope for deletion here, but if docs reference it, demoting it to `pub(crate)` is the right call.)
   - Is it a `Default::default()`, `Display`, `From`, `Serialize`/`Deserialize` impl that might be invoked indirectly by serde/format machinery? Grep for the trait's full path: `rg 'impl Display for <Type>'` and `rg '<Type>: Display'` etc.
4. Make the call: delete, demote to `pub(crate)`, or keep.
5. Each deletion gets one sentence in the commit body: "X was orphan because Y" or "X demoted to pub(crate) because Y uses it elsewhere".

**Crate order:** same as Pass 1 — leaves first, then `hermes-agent` (largest, most internal pub surface), then `hermes-cli`.

**Output per crate:** one or more commits. For very large surfaces (e.g. `hermes-agent` lib.rs re-exports, `tui/`), commit per logical group (e.g. "remove orphan tui events", "shrink agent pub re-exports").

**Likely hotspots** (from line counts and CLAUDE.md):
- `hermes-providers/src/anthropic.rs` (896 lines) — many request/response helpers
- `hermes-cli/src/tui/render.rs` (581 lines) — pub helpers for rendering widgets
- `hermes-agent/src/loop_engine.rs` (563 lines) — pub event types
- `hermes-agent/src/runtime_agent.rs` (484 lines) — pub `AIAgent` API

### Pass 4 — Redundant Logic

**Tooling:**
- `cargo clippy --workspace --all-targets --all-features -- -W clippy::pedantic -W clippy::nursery 2>&1 | tee clippy-pedantic.log`

**Lints to address (in priority order):**
1. `redundant_clone`
2. `redundant_locals`
3. `or_fun_call`
4. `needless_pass_by_value`
5. `single_match` and `single_match_else`
6. `redundant_pattern_matching`
7. `if_let_some_else_none`
8. `redundant_closure`
9. `manual_let_else`
10. `needless_return`

**Method:** for each lint, locate the code, read context (e.g. is the `clone()` actually needed because the value's `Drop` does work?), then apply the suggested fix. Reject lints that fight the codebase's existing style (we don't refactor unrelated style here).

**Skip these as too noisy for this pass:** `clippy::module_name_repetitions`, `clippy::similar_names`, `clippy::too_many_arguments`, `clippy::cognitive_complexity`. These are style lints, not redundancy.

**Risk:** medium. The `clone()` family in particular is easy to "fix" wrongly. Always re-run `cargo test` after each batch of changes.

### Pass 5 — tests/ and examples/ Helpers

**Tooling:**
- `rg` for helper usage
- `cargo test --workspace` and `cargo run -p hermes-providers --example <name>` per example

**Method:**

1. For each `tests/support/*.rs` file, list top-level `pub fn`, `pub struct`, etc. Grep the workspace for usage. Orphan helpers are deleted.
2. For each `tests/*.rs` file, look for: tests that are skipped via `#[ignore]` with no `#[ignore = "reason"]` and no obvious gate, tests that just assert a constant, tests that duplicate another test.
3. For each `examples/*.rs` file, confirm it `cargo run`s successfully. Examples that don't compile or that exist only as documentation are removed.
4. For `dev-dependencies` that were only used by deleted tests, remove them (this overlaps with Pass 1 — defer to Pass 1's report).

**Risk:** low. Tests are isolated. If a test is removed and a regression occurs later, the test can be re-added with a clear repro.

## Verification Gates

Run after **every commit**, not just at the end of each pass:

1. `cargo test --workspace` — must be green.
2. `cargo clippy --all-targets --all-features -- -D warnings` — must be clean.
3. `cargo fmt --all -- --check` — must be clean (cosmetic, blocks the commit if violated).

Run additionally **at the end of each pass** (before the pass is declared done):

4. `cargo doc --workspace --no-deps` — must build with no warnings about missing doc links.
5. `wc -l crates/*/src/**/*.rs` — record line count; the pass is "done" only if line count dropped or held steady (a meaningful *increase* signals something went wrong).

## Out of Scope (Hard)

- Repo-root binaries (`rust_out`, etc.) and any non-Rust cruft at the repo root.
- `docs/` content beyond fixing broken doc-links caused by Pass 3 removals.
- `examples/config/hermes.toml` and `/Users/amagicpear/.perry_hermes/config.toml` content (the file format is owned by Pass 3's removal decisions only insofar as a key becomes unused).
- `CLAUDE.md`, `README.md` body content (only update if a documented feature is removed).
- Any new tests, new error variants, new trait impls.
- Any change to runtime behavior. If you find a "while I'm here" simplification, file it for a follow-up — do not bundle into this work.

## Commit Discipline

- One commit per logical unit of deletion. Resist the urge to lump.
- Commit body must contain:
  - **What** was removed (file paths, item names).
  - **Why** it's safe to remove (one sentence per the depth-of-understanding principle).
  - **Verification** confirmation ("`cargo test --workspace` and `cargo clippy --all-targets --all-features -- -D warnings` both green after this commit").
- Commit prefix: `chore(cleanup):` for passes 1-2, `refactor(cleanup):` for passes 3-5 (the latter carries meaning beyond surface deletion).

## Risk Register

| Risk | Mitigation |
|---|---|
| Removing a `pub` item that an unbuilt example or future test would need | `cargo run --example <name>` for every example in the workspace at the end of Pass 3 |
| `cargo +nightly udeps` not available (no nightly toolchain) | Fallback: manual audit of `[dev-dependencies]` and `[build-dependencies]` sections, crate by crate |
| Removing a `Default` impl that some `#[serde(default)]` indirectly relies on | Grep for `<Type>: Default` and the impl block, plus `#[serde(default` attributes referencing the type |
| A lint suggestion is wrong (e.g. `clone()` removal causes double-free) | Re-run `cargo test` after each batch; if a test fails, revert the batch |
| Cross-crate pub re-exports cascade — removing one item forces 5 follow-up edits | Allowed: the follow-ups go in the same logical commit |
| `pub trait` method removal breaks a blanket impl in tests | Pass 3's per-trait read step catches this; verify by re-running tests after the trait's commit |

## Deliverable Shape

The work is complete when:

1. All five passes are done, with their commits merged to `main`.
2. The line-count delta from before Pass 1 to after Pass 5 is recorded in the final commit body (just before/after `wc -l` of `crates/*/src/`).
3. `cargo test --workspace` is green.
4. `cargo clippy --all-targets --all-features -- -D warnings` is clean.
5. `cargo doc --workspace --no-deps` builds without broken links.

No standalone report doc is produced. The commit history is the only record; the per-commit bodies already contain the "what" and "why" for each removal.
