# Tool Contract Alignment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Align Perry Hermes's foundational model-visible tool contracts with `~/.hermes/hermes-agent` and add the missing `patch` and `search_files` tools.

**Architecture:** Keep the current manual registry for this pass. Extend the core `Tool` trait with default metadata methods, then add focused file-tool modules under `crates/hermes-agent/src/tools/files/` that reuse existing path resolution and write safety policy.

**Tech Stack:** Rust 2021, async-trait, serde_json, tokio, tempfile, existing `Tool`/`InMemoryRegistry` APIs.

---

## File Structure

- Modify `crates/hermes-core/src/tool.rs`: add default metadata methods.
- Modify `crates/hermes-core/src/registry.rs`: include metadata in `ToolSchema`.
- Modify `crates/hermes-agent/src/tools/files/mod.rs`: export new file tools and share policy helpers.
- Create `crates/hermes-agent/src/tools/files/patch.rs`: implement `patch` reference schema, replace mode, and V4A mode.
- Create `crates/hermes-agent/src/tools/files/search.rs`: implement `search_files` reference schema.
- Modify `crates/hermes-agent/src/tools/files/write.rs`: add `files_modified` on success.
- Modify `crates/hermes-agent/src/tools/mod.rs`: re-export `PatchTool` and `SearchFilesTool`.
- Modify `crates/hermes-agent/src/tool_catalog.rs`: register `patch` and `search_files`.
- Modify `crates/hermes-agent/tests/files.rs`: add behavior tests for patch/search/write return shape.
- Modify `crates/hermes-agent/src/tool_catalog.rs` tests: assert new tools are registered and filtered by `file`.

## Tasks

### Task 1: Core Tool Metadata

**Files:**
- Modify: `crates/hermes-core/src/tool.rs`
- Modify: `crates/hermes-core/src/registry.rs`

- [ ] **Step 1: Write failing registry metadata assertions**

Add a test tool in `crates/hermes-core/src/registry.rs` tests whose metadata returns non-default values, then assert `schemas()` includes them:

```rust
fn is_async(&self) -> bool { true }
fn requires_env(&self) -> &[&str] { &["DEMO_ENV"] }
fn max_result_size_chars(&self) -> Option<usize> { Some(1234) }
fn emoji(&self) -> Option<&str> { Some("T") }
fn check_available(&self) -> bool { false }
```

Run:

```bash
cargo test -p perry-hermes-core registry::tests::register_lookup_and_schema
```

Expected: fail because `ToolSchema` has no metadata fields.

- [ ] **Step 2: Add metadata defaults and schema fields**

Add default methods to `Tool`:

```rust
fn is_async(&self) -> bool { false }
fn requires_env(&self) -> &[&str] { &[] }
fn max_result_size_chars(&self) -> Option<usize> { None }
fn emoji(&self) -> Option<&str> { None }
fn check_available(&self) -> bool { true }
```

Add fields to `ToolSchema`:

```rust
pub toolset: String,
pub is_async: bool,
pub requires_env: Vec<String>,
pub max_result_size_chars: Option<usize>,
pub emoji: Option<String>,
pub available: bool,
```

- [ ] **Step 3: Verify core tests pass**

Run:

```bash
cargo test -p perry-hermes-core
```

Expected: pass.

### Task 2: `patch` Tool

**Files:**
- Create: `crates/hermes-agent/src/tools/files/patch.rs`
- Modify: `crates/hermes-agent/src/tools/files/mod.rs`
- Modify: `crates/hermes-agent/src/tools/mod.rs`
- Modify: `crates/hermes-agent/src/tool_catalog.rs`
- Modify: `crates/hermes-agent/tests/files.rs`

- [ ] **Step 1: Write failing tests**

Add tests for:

- schema name is `patch` and required contains only `mode`
- replace mode edits a unique string
- replace mode rejects duplicate matches without `replace_all`
- replace mode supports `replace_all`
- V4A add/update/delete/move works
- cross-profile writes are rejected unless `cross_profile=true`

Run:

```bash
cargo test -p perry-hermes-agent --test files patch_
```

Expected: fail because `PatchTool` does not exist.

- [ ] **Step 2: Implement minimal patch tool**

Implement `PatchTool` with the reference schema. Reuse `resolve_user_path`,
`sensitive_write_path_message`, `cross_profile_write_message`, `temp_sibling`,
and `is_internal_file_status_text`.

Replace mode:

- require `path`, `old_string`, `new_string`
- read file as UTF-8 text
- count matches of `old_string`
- reject zero matches with JSON `{"error": "...", "_hint": "..."}`
- reject multiple matches unless `replace_all=true`
- write atomically
- return JSON with `files_modified`, `resolved_path`, and `diff`

V4A mode:

- parse `*** Begin Patch` / `*** End Patch`
- support `*** Add File:`, `*** Update File:`, `*** Delete File:`,
  `*** Move File:`, and optional `*** Move to:`
- apply line hunks using exact old text assembled from ` ` and `-` lines
- return JSON with `files_modified` and operation results

- [ ] **Step 3: Register and export**

Export `PatchTool` from `files/mod.rs` and `tools/mod.rs`, then register it
in `build_registry` under the `file` toolset.

- [ ] **Step 4: Verify patch tests pass**

Run:

```bash
cargo test -p perry-hermes-agent --test files patch_
```

Expected: pass.

### Task 3: `search_files` Tool

**Files:**
- Create: `crates/hermes-agent/src/tools/files/search.rs`
- Modify: `crates/hermes-agent/src/tools/files/mod.rs`
- Modify: `crates/hermes-agent/src/tools/mod.rs`
- Modify: `crates/hermes-agent/src/tool_catalog.rs`
- Modify: `crates/hermes-agent/tests/files.rs`

- [ ] **Step 1: Write failing tests**

Add tests for:

- schema name is `search_files`
- content search returns path, line, column, and content
- `file_glob` restricts content search
- `offset` and `limit` paginate content results
- `output_mode="files_only"` returns only file paths
- `output_mode="count"` returns counts per file
- `target="files"` finds files by glob sorted by modification time

Run:

```bash
cargo test -p perry-hermes-agent --test files search_files_
```

Expected: fail because `SearchFilesTool` does not exist.

- [ ] **Step 2: Implement minimal search tool**

Implement `SearchFilesTool` with the reference schema.

Content search:

- recursively walk from `path`
- skip binary extensions with existing policy
- filter by `file_glob` using simple `*` wildcard matching
- treat `pattern` as literal substring for first pass
- return JSON with `matches`, `total`, `truncated`

Files search:

- recursively walk from `path`
- match file names against `pattern` using simple `*` wildcard matching
- sort by modification time descending
- return JSON with `files`, `total`, `truncated`

- [ ] **Step 3: Register and export**

Export `SearchFilesTool` and register it under the `file` toolset.

- [ ] **Step 4: Verify search tests pass**

Run:

```bash
cargo test -p perry-hermes-agent --test files search_files_
```

Expected: pass.

### Task 4: Registry Contract Tests

**Files:**
- Modify: `crates/hermes-agent/src/tool_catalog.rs`
- Modify: `crates/hermes-agent/tests/files.rs`

- [ ] **Step 1: Add registry assertions**

Update `default_registry_includes_all_five_tools` to assert seven tools:
`terminal`, `read_file`, `write_file`, `patch`, `search_files`,
`skills_list`, and `skill_view`.

Update `file_toolset_disables_read_and_write` to assert it removes all four
file tools.

- [ ] **Step 2: Add schema compatibility tests**

Assert `patch` parameters contain the reference keys and `search_files`
parameters contain the reference keys.

- [ ] **Step 3: Verify focused tests**

Run:

```bash
cargo test -p perry-hermes-agent tool_catalog
cargo test -p perry-hermes-agent --test files
```

Expected: pass.

### Task 5: Final Verification

**Files:**
- Modify: none unless verification reveals issues.

- [ ] **Step 1: Format**

Run:

```bash
cargo fmt --all
```

Expected: no unformatted files remain.

- [ ] **Step 2: Workspace tests**

Run:

```bash
cargo test --workspace
```

Expected: pass.

- [ ] **Step 3: Clippy**

Run:

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: pass.

- [ ] **Step 4: Docs**

Run:

```bash
cargo doc --no-deps
```

Expected: pass.
