# Generalize System-Prompt Injection via `PromptContextBlock`

**Date:** 2026-06-10
**Status:** Draft

## Goal

Generalize the current hard-coded `AGENTS.md` injection in
`hermes-agent::prompting` into a uniform `PromptContextBlock` trait
that supports both **project-level** sources (resolved relative to the
session's `working_dir`) and **global** sources (resolved relative to
`PERRY_HERMES_HOME`).

The first concrete consumers of the new abstraction are:

- **Project-level:** `AGENTS.md` (existing behavior, refactored to the
  new trait).
- **Global:** `MEMORY.md` and `USER.md` (new). These introduce a
  `memory` tool that mutates the on-disk files.

Every block follows the same contract: a `name()` label, an async
`load()` that returns the rendered body (or `None` to skip), and a
shared `build_system_message` that walks the block list at session
creation. The order of blocks is determined at the call site, so
project and global sources can be interleaved as the operator chooses.

This work exists **to enable** memory + future per-profile files
(`SOUL.md`, `INSTRUCTIONS.md`, etc.) without further refactoring. The
refactor itself is the goal; memory is its first beneficiary.

## Non-Goals

- No memory provider plugin system (Honcho, Mem0, etc.). The built-in
  file-backed store is the only backend.
- No TOML configuration block for memory. The tool is always
  registered; users disable via `disabled_toolsets = ["memory"]` like
  every other toolset.
- No character-limit / capacity enforcement. Stores are unbounded.
- No drift detection (the hermes-agent "external writer corrupted file"
  guard). YAGNI: perry_hermes has no patch-tool / shell-append path
  that writes MEMORY.md today.
- No async / background prefetch, no system-prompt scrubbing of fenced
  `<memory-context>` blocks.
- **No semantic changes to existing `AGENTS.md` behavior.** The
  refactored path produces byte-identical output for the same inputs.
- No new public API for users to register custom blocks. The block
  list is a constructor argument, not a runtime registry. Operators
  add a new block by adding a new `impl PromptContextBlock` and
  wiring it into the call sites.

## Architecture

```
hermes-core                  (no IO, no async beyond trait signatures)
  └─ prompt_context.rs       PromptContextBlock trait (new)

hermes-skill-tools           (tool implementations + IO)
  └─ tools/memory/           (new — first non-AGENTS block)
       ├─ mod.rs
       ├─ store.rs           MemoryStore, MemoryConfig, LiveState
       └─ tool.rs            MemoryTool

hermes-agent                 (assembly)
  ├─ prompting.rs            Refactored: AgentsMdBlock (project-level),
  │                          build_system_message takes blocks list
  └─ tool_catalog.rs         MemoryTool registration (default on)
```

`PromptContextBlock` lives in `hermes-core` because the trait is a
pure contract — no I/O, no agent-specific knowledge. The implementations
(`AgentsMdBlock`, `MemoryBlock`) live in the crate that already owns
their I/O:

- `AgentsMdBlock` stays in `hermes-agent::prompting` because the
  existing `load_agents_md_block` helper and the `AGENTS_MD_FILENAME`
  constant are already there.
- `MemoryBlock` lives in `hermes-agent::prompting` too, alongside
  `AgentsMdBlock`, even though its underlying store (`MemoryStore`)
  is in `hermes-skill-tools`. Rationale: the block layer is
  presentation (render entries as a system-prompt block); the store
  is data + I/O. They are separate concerns, and putting the
  presentation block next to `AgentsMdBlock` keeps the "what blocks
  exist" picture in one file.

## Component 1: `PromptContextBlock` trait (`hermes-core`)

```rust
// crates/hermes-core/src/prompt_context.rs

use async_trait::async_trait;

/// A context fragment loaded at session creation and frozen into the
/// system prompt. Implementations own their I/O.
///
/// Two natural categories:
///
/// - **Project-level blocks** resolve their backing file relative to
///   the session's `working_dir` (e.g. `AGENTS.md`, future `CLAUDE.md`).
/// - **Global blocks** resolve relative to the active
///   `PERRY_HERMES_HOME` (e.g. `MEMORY.md`, `USER.md`, future
///   `SOUL.md`).
///
/// All blocks share the same contract: a label, an async load, and an
/// optional body. Callers iterate the block list once per session at
/// `build_system_message` time. The result is stored on
/// `AgentSession.system_message` and treated as immutable for the
/// session's lifetime.
///
/// Returning `None` from `load()` skips injection — the canonical
/// case is "the backing file does not exist or is empty". I/O errors
/// should be logged via `tracing::warn!` and treated as `None`; a
/// missing or unreadable file must not crash the agent.
#[async_trait]
pub trait PromptContextBlock: Send + Sync {
    /// Stable label used as the block header. Examples: "AGENTS.md",
    /// "MEMORY", "USER". The label is shown above the body in the
    /// rendered system prompt.
    fn name(&self) -> &str;

    /// Load and render the block. `None` → caller skips this block.
    async fn load(&self) -> Option<String>;
}
```

Re-exported from `hermes_core::lib`.

## Component 2: Refactored `prompting.rs` (`hermes-agent`)

### `load_agents_md_block` stays

The existing `load_agents_md_block(working_dir: &Path) -> Option<String>`
function is preserved as a sync helper, with the same body. No
semantic change. It now exists to back `AgentsMdBlock::load`.

### New: `AgentsMdBlock`

```rust
// crates/hermes-agent/src/prompting.rs

pub struct AgentsMdBlock {
    working_dir: PathBuf,
}

impl AgentsMdBlock {
    pub fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }
}

#[async_trait]
impl PromptContextBlock for AgentsMdBlock {
    fn name(&self) -> &str { "AGENTS.md" }
    async fn load(&self) -> Option<String> {
        // Sync I/O on a small file; no contention.
        load_agents_md_block(&self.working_dir)
    }
}
```

`AgentsMdBlock` produces the same output as the current
`load_agents_md_block` (a body like
`"Project guidance from `AGENTS.md`:\n\n{trimmed body}"`). The
`{name}\n\n{body}` wrapper is added by `build_system_message`, so
`AgentsMdBlock::load` returns just the post-label content.

### `build_system_message` signature change

```rust
pub async fn build_system_message(
    working_dir: &Path,
    blocks: &[Arc<dyn PromptContextBlock>],
) -> Option<Message> {
    let mut sections: Vec<String> = Vec::with_capacity(blocks.len() + 2);
    if let Some(base) = compose_session_prompt_prefix() {
        sections.push(base.trim().to_string());
    }
    for block in blocks {
        if let Some(body) = block.load().await {
            sections.push(format!("{}\n\n{}", block.name(), body));
        }
    }
    sections.push(working_directory_hint(working_dir));

    if sections.is_empty() {
        None
    } else {
        Some(Message::system(sections.join("\n\n")))
    }
}
```

### Backward compatibility

The existing function `load_agents_md_block` and constant
`AGENTS_MD_FILENAME` remain public — no breaking change for any
external caller. `build_system_message` is the only signature change
in `prompting.rs`. The existing test functions
(`build_system_message_includes_working_dir_even_without_agents`,
`build_system_message_includes_default_base_prompt_and_working_dir`,
`build_system_message_orders_base_agents_md_working_dir`,
`build_system_message_omits_agents_block_when_file_missing`,
`build_system_message_reads_agents_md_from_session_working_dir_not_process_cwd`)
are converted from `#[test]` to `#[tokio::test]`, pass an empty blocks
slice, and `.await` the result. Their assertions stay valid because
the AGENTS.md path still produces the same byte content for the same
inputs.

## Component 3: `MemoryStore` (`hermes-skill-tools::tools::memory::store`)

```rust
pub const ENTRY_DELIMITER: &str = "\n§\n";

pub struct MemoryConfig {
    /// Directory containing `MEMORY.md` and `USER.md`. Resolved by
    /// the caller from `PERRY_HERMES_HOME` / `$HOME/.perry_hermes` /
    /// `./.perry_hermes` (same rules as `prompting::resolve_skills_dir`).
    pub memories_dir: PathBuf,
}

pub struct MemoryStore {
    cfg: MemoryConfig,
    state: Arc<RwLock<LiveState>>,
}

struct LiveState {
    memory_entries: Vec<String>,
    user_entries: Vec<String>,
}

impl MemoryStore {
    /// Read MEMORY.md and USER.md from disk, parse entries, return
    /// the populated store. Missing files are treated as empty stores,
    /// not errors.
    pub async fn load(cfg: MemoryConfig) -> std::io::Result<Self>;

    /// Read-only view of live entries. Used by `MemoryBlock::load`.
    /// `MemoryBlock` calls this once per session, so the rendered
    /// result is effectively frozen for the session's lifetime.
    pub async fn entries(&self, target: MemoryTarget) -> Vec<String>;

    // Mutators, called by MemoryTool.
    pub async fn add(&self, target, content) -> Result<MemoryOpResult, MemoryError>;
    pub async fn replace(&self, target, old, new) -> Result<MemoryOpResult, MemoryError>;
    pub async fn remove(&self, target, old) -> Result<MemoryOpResult, MemoryError>;
    pub async fn read(&self, target) -> Result<MemoryReadResult, MemoryError>;
}
```

### `MemoryTarget` lives in `hermes-skill-tools`, not `hermes-core`

The target enum is part of the store / tool public surface, not a
generic agent concept. Keeping it in `hermes-skill_tools::tools::memory`
matches the existing pattern (tools own their enums).

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryTarget { Memory, User }
```

### Storage format

Two plain-text files under `memories_dir`, entries joined by `§`.
No frontmatter, no headers, no usage indicators.

```
# MEMORY.md example content
prefer cargo over rustc directly
homebrew package manager on this mac
postgres runs on port 5432, credentials in ~/.pgpass
```

The `§` delimiter is the hermes-agent convention; matches user
expectations from existing `read_file` / `write_file` tools.

### Concurrency

`flock`-style exclusive file lock for read-modify-write;
`tempfile + std::fs::rename` for atomic replace. The lock file lives
at `{path}.lock`. Readers (`entries()`) and `load()` do not need a
lock — the rename pattern guarantees readers see complete files.

### Mutator semantics

- `add`:
  - Reject empty / whitespace-only `content`.
  - Skip exact duplicates.
  - Append to live state, write to disk under file lock.
- `replace`:
  - Find all entries containing `old` as a substring.
  - Zero matches → `NoMatch`.
  - Multiple matches with **different** bodies → `AmbiguousMatch`.
  - Multiple matches with **identical** bodies → operate on first.
  - One match → replace in place.
- `remove`: same matching rules, no `new` argument.
- `read`: return all current entries, no mutation.

All mutators return `MemoryOpResult { target, entries, entry_count, message? }`.

## Component 4: `MemoryTool` (`hermes-skill-tools::tools::memory::tool`)

```rust
pub struct MemoryTool {
    store: Arc<MemoryStore>,
}

impl MemoryTool {
    pub fn new(store: Arc<MemoryStore>) -> Self;
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str { "memory" }
    fn toolset(&self) -> &'static str { "memory" }
    fn description(&self) -> &str { /* adapted from hermes-agent */ }
    fn parameters_schema(&self) -> Value { /* action, target, content, old_text */ }
    fn emoji(&self) -> Option<&str> { Some("🧠") }

    async fn execute(&self, args, _ctx, _cancel) -> Result<ToolOutput, ToolError> {
        // Parse action + target, dispatch to store, serialize result.
    }
}
```

### Schema

```json
{
  "type": "object",
  "properties": {
    "action": { "type": "string", "enum": ["add", "replace", "remove", "read"] },
    "target": { "type": "string", "enum": ["memory", "user"] },
    "content": { "type": "string", "description": "Required for add and replace." },
    "old_text": { "type": "string", "description": "Required for replace and remove. Short unique substring of the target entry." }
  },
  "required": ["action", "target"]
}
```

### Tool description

Preserve the behavioral guidance from hermes-agent's `MEMORY_SCHEMA`
description (WHEN TO SAVE, priority, TWO TARGETS, ACTIONS, SKIP).
Edit only the language to fit perry_hermes's voice; the contract
stays.

### Tool result format

Success: `{"success": true, "target": "memory", "entries": [...], "entry_count": 3, "message": "Entry added."}`
Error:   `{"success": false, "error": "no entry matched 'foo'"}`

## Component 5: `MemoryBlock` (`hermes-agent::prompting`)

```rust
// crates/hermes-agent/src/prompting.rs

pub struct MemoryBlock {
    store: Arc<MemoryStore>,
    target: MemoryTarget,
    name_label: &'static str,
}

impl MemoryBlock {
    pub fn memory(store: Arc<MemoryStore>) -> Self {
        Self { store, target: MemoryTarget::Memory, name_label: "MEMORY" }
    }
    pub fn user(store: Arc<MemoryStore>) -> Self {
        Self { store, target: MemoryTarget::User, name_label: "USER" }
    }
}

#[async_trait]
impl PromptContextBlock for MemoryBlock {
    fn name(&self) -> &str { self.name_label }
    async fn load(&self) -> Option<String> {
        let entries = self.store.entries(self.target).await;
        if entries.is_empty() { return None; }
        Some(entries.join("\n\n"))
    }
}
```

`MemoryBlock` reads from in-memory `state` (not disk). Since the
agent calls `build_system_message` once per session, the rendered
block is effectively immutable for the session's lifetime.

## Component 6: Block assembly

The call site (CLI / TUI / gateway binary) constructs the block list:

```rust
let memory_store = Arc::new(
    MemoryStore::load(MemoryConfig {
        memories_dir: resolve_memories_dir(),
    }).await?
);

let blocks: Vec<Arc<dyn PromptContextBlock>> = vec![
    Arc::new(AgentsMdBlock::new(working_dir.clone())),
    Arc::new(MemoryBlock::memory(memory_store.clone())),
    Arc::new(MemoryBlock::user(memory_store.clone())),
];

let registry = build_registry(
    disabled_toolsets,
    &skills_dir,
    memory_store.clone(),  // for MemoryTool
);
let agent = AgentLoop::new(
    provider,
    registry,
    LoopConfig {
        blocks: blocks.clone(),
        // ... existing fields
    },
);
```

`AgentLoop` stores the blocks and passes them to
`build_system_message` whenever it constructs a new session. This is
the only signature change in `AgentLoop::new_session`.

## Configuration

No `[memory]` TOML block. The tool is registered by default; users
disable via the existing `disabled_toolsets` list:

```toml
[agent]
disabled_toolsets = ["memory"]
```

`memories_dir` is resolved at runtime from the same rules as
`resolve_skills_dir`:

1. `$PERRY_HERMES_HOME/memories`
2. `$HOME/.perry_hermes/memories`
3. `./.perry_hermes/memories`

A new resolver `resolve_memories_dir()` lives next to
`resolve_skills_dir` in `prompting.rs`.

## Error Handling

| Situation | Surface |
|-----------|---------|
| `MEMORY.md` / `USER.md` missing at load | empty store, no warning |
| `MEMORY.md` / `USER.md` permission error at load | `tracing::warn!`, empty store |
| `MemoryStore` mutator with invalid target | `MemoryError::InvalidTarget` |
| `add` with empty content | `MemoryError::MissingContent` |
| `replace` / `remove` with no match | `MemoryError::NoMatch` |
| `replace` / `remove` with ambiguous match | `MemoryError::AmbiguousMatch` |
| File lock contention (e.g. sister session) | block on flock; no timeout |
| Disk write failure | `MemoryError::Io`, surfaced to model |
| `PromptContextBlock::load` error | logged, treated as `None` |
| `MemoryTool` invoked with missing required field | `ToolError::InvalidArgs` |

The model never sees an unwrapped IO error; it gets the JSON
`{success: false, error: "..."}` shape and can retry or change
approach.

## Testing

### `PromptContextBlock` trait

Pure contract; no separate tests.

### `AgentsMdBlock` (`hermes-agent::prompting`)

- `name()` returns "AGENTS.md"
- `load()` returns `None` when file missing
- `load()` returns the same body that `load_agents_md_block` returns
  for the same inputs (existing helper is the body producer; the
  block is the wrapper)

### `MemoryStore` (`hermes-skill-tools`)

- `load` with missing `MEMORY.md` returns empty store
- `load` with missing `USER.md` returns empty store
- `load` with both files populates `entries()` correctly
- `add` appends, returns success
- `add` with exact duplicate returns success with "no duplicate" message
- `add` with empty content returns `MissingContent`
- `replace` with one match substitutes in place
- `replace` with zero matches returns `NoMatch`
- `replace` with multiple **different** matches returns `AmbiguousMatch`
- `replace` with multiple **identical** matches replaces first
- `remove` with one match deletes
- `remove` with no match returns `NoMatch`
- `entries()` after mutator reflects new state
- Two `Arc<MemoryStore>` instances loaded from the same dir, both
  calling `add` concurrently → final state contains both entries
  (Unix-only, gated on platform)
- After `add`, the on-disk file uses the `§` delimiter and round-trips
  through `load`

### `MemoryTool` (`hermes-skill-tools`)

- All four actions dispatch correctly
- Success responses are JSON `{success: true, target, entries, entry_count, message?}`
- Error responses are JSON `{success: false, error: "..."}`
- Missing `action` returns `ToolError::InvalidArgs`
- Missing required field per action returns `ToolError::InvalidArgs`

### `MemoryBlock` (`hermes-agent::prompting`)

- `name()` returns "MEMORY" or "USER" per constructor
- `load()` returns `None` when store is empty
- `load()` returns `"{entry1}\n\n{entry2}"` style joined body

### `build_system_message` (`hermes-agent::prompting`)

Existing tests converted to `#[tokio::test]`, `.await` the call,
pass empty blocks slice, all assertions remain valid.

New tests:

- Empty blocks list returns the base prompt + working-dir hint only
- Block order in `sections` matches the order in the input slice
- Block returning `None` is silently skipped
- Working-directory hint is always the last section
- Mixed project + global blocks render in caller-specified order

## File-by-file change summary

| File | Status | Notes |
|------|--------|-------|
| `crates/hermes-core/src/prompt_context.rs` | new | trait only |
| `crates/hermes-core/src/lib.rs` | edit | add `prompt_context` module + re-export |
| `crates/hermes-skill-tools/src/tools/memory/mod.rs` | new | re-exports |
| `crates/hermes-skill-tools/src/tools/memory/store.rs` | new | `MemoryStore`, `MemoryConfig`, `MemoryOpResult`, `MemoryReadResult`, `MemoryTarget` |
| `crates/hermes-skill-tools/src/tools/memory/tool.rs` | new | `MemoryTool` |
| `crates/hermes-skill-tools/src/tools/mod.rs` | edit | add `pub mod memory;` |
| `crates/hermes-skill-tools/Cargo.toml` | edit | add `fs2` for cross-platform flock, `tempfile` |
| `crates/hermes-agent/src/prompting.rs` | edit | `AgentsMdBlock`, `MemoryBlock`, `build_system_message(blocks)`, `resolve_memories_dir`; convert existing tests to `#[tokio::test]` |
| `crates/hermes-agent/src/tool_catalog.rs` | edit | `build_registry` accepts `Arc<MemoryStore>`, registers `MemoryTool` |
| `crates/hermes-agent/src/loop_engine/agent_loop.rs` | edit | `AgentLoop` holds `blocks: Vec<Arc<dyn PromptContextBlock>>`, passes to `build_system_message` |
| CLI / TUI / gateway binaries | edit | construct `MemoryStore`, build blocks list, pass to `AgentLoop::new` |

## Out of Scope (Future Work)

These are intentionally NOT part of this design.

- Memory provider plugin system (Honcho, Mem0, etc.).
- Auto-summarization / extraction on session end.
- Per-user / per-profile isolation beyond what `$PERRY_HERMES_HOME`
  already provides.
- Embedding-based semantic recall.
- Drift detection.
- Capacity / character limits.
- `[memory]` TOML configuration block.
- Runtime block registry (the block list is a constructor argument;
  adding a new block type means editing the call sites).
- A "block discovery" mechanism (scanning `working_dir` for any
  `*.md` and treating them as blocks). Each new source still needs
  an explicit `impl PromptContextBlock` and an explicit entry in the
  block list.
