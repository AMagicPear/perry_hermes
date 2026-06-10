# Built-in Memory Tool

**Date:** 2026-06-10
**Status:** Draft

## Goal

Give the agent persistent, file-backed memory that survives across sessions
and is shared across CLI / gateway / subagent runtimes. Follow the
hermes-agent `MemoryStore` shape (MEMORY.md + USER.md) but adapt to
perry_hermes's architecture.

Two storage files, one `memory` tool with `add` / `replace` / `remove` /
`read` actions, and unified system-prompt injection through a new
`PromptContextBlock` trait so AGENTS.md, MEMORY, and USER are
architecturally equivalent.

## Non-Goals

- No memory provider plugin system (Honcho, Mem0, etc.). The built-in
  file-backed store is the only backend.
- No TOML configuration block. The tool is always registered; users
  disable via `disabled_toolsets = ["memory"]` like every other toolset.
- No character-limit / capacity enforcement. Stores are unbounded; the
  frozen snapshot is truncated only by the model context window.
- No drift detection (the hermes-agent "external writer corrupted file"
  guard). YAGNI: perry_hermes has no patch-tool / shell-append path
  that writes MEMORY.md today.
- No async / background prefetch, no system-prompt scrubbing of fenced
  `<memory-context>` blocks. The built-in store reads its own files;
  there's no streaming recall to scrub.

## Architecture

Three layers, mirroring the existing workspace structure.

```
hermes-core                  (no IO, no async beyond trait signatures)
  └─ prompt_context.rs       PromptContextBlock trait
  └─ memory.rs               MemoryTarget enum, MemoryError

hermes-skill-tools           (tool implementations + IO)
  └─ tools/memory/
       ├─ mod.rs             re-exports
       ├─ store.rs           MemoryStore, MemoryConfig, LiveState
       └─ tool.rs            MemoryTool, MemoryOpResult, MemoryReadResult

hermes-agent                 (assembly)
  ├─ prompting.rs            AgentsMdBlock impl, build_system_message
  │                          takes blocks list
  └─ tool_catalog.rs         MemoryTool registration (default on)
```

The `PromptContextBlock` trait makes AGENTS.md, MEMORY, and USER
architecturally equivalent: same `name() -> &str` + `load() -> Option<String>`
contract, same `build_system_message` consumption path, same
`{name}\n\n{body}` rendering. The differences are entirely inside each
implementation: where the file lives, how the body is composed, and
which tool (if any) mutates it.

## Component 1: `PromptContextBlock` trait (`hermes-core`)

```rust
// crates/hermes-core/src/prompt_context.rs

use async_trait::async_trait;

/// A context fragment loaded at session creation and frozen into the
/// system prompt. Implementations own their I/O.
///
/// AGENTS.md, MEMORY, and USER are all examples. The block is
/// responsible for its own file resolution, parsing, and rendering.
/// Returns `None` from `load()` to skip injection — the canonical case
/// is "the backing file does not exist or is empty".
#[async_trait]
pub trait PromptContextBlock: Send + Sync {
    /// Stable identifier used as the block's label in the rendered
    /// system prompt. Examples: "AGENTS.md", "MEMORY", "USER".
    fn name(&self) -> &str;

    /// Load and render the block. `None` → caller skips this block.
    /// Errors are logged via `tracing::warn!` and treated as `None`;
    /// a missing or unreadable file must not crash the agent.
    async fn load(&self) -> Option<String>;
}
```

Re-exported from `hermes_core::lib`.

## Component 2: `MemoryTarget` + `MemoryError` (`hermes-core`)

```rust
// crates/hermes-core/src/memory.rs

use serde::Serialize;

/// Two parallel stores: the agent's own notes and what it knows about
/// the user. Serialized in tool schemas and tool results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryTarget { Memory, User }

/// Stable error shape sent back to the model. The serialized form
/// (tag = "kind", content = "message") is the public contract; any
/// addition must remain backward-compatible.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum MemoryError {
    #[error("target must be 'memory' or 'user'")]
    InvalidTarget,
    #[error("content is required")]
    MissingContent,
    #[error("no entry matched '{0}'")]
    NoMatch(String),
    #[error("multiple entries matched '{0}'; be more specific")]
    AmbiguousMatch(String),
    #[error("io error: {0}")]
    Io(String),
}
```

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
    /// Read MEMORY.md and USER.md from disk under an exclusive flock,
    /// parse entries, and return the populated store. Missing files
    /// are treated as empty stores, not errors.
    pub async fn load(cfg: MemoryConfig) -> std::io::Result<Self>;

    /// Read-only view of the live entries. Used by `MemoryBlock::load`
    /// to render the system-prompt snapshot. `MemoryBlock` calls this
    /// once per session, so the rendered result is effectively frozen
    /// for the session's lifetime.
    pub async fn entries(&self, target: MemoryTarget) -> Vec<String>;

    // Mutators, called by MemoryTool.
    pub async fn add(&self, target, content) -> Result<MemoryOpResult, MemoryError>;
    pub async fn replace(&self, target, old, new) -> Result<MemoryOpResult, MemoryError>;
    pub async fn remove(&self, target, old) -> Result<MemoryOpResult, MemoryError>;
    pub async fn read(&self, target) -> Result<MemoryReadResult, MemoryError>;
}
```

### Storage format

Two plain-text files under `memories_dir`, one entry per line, joined
by `§`. The `§` delimiter is the hermes-agent convention and matches
existing `read_file` / `write_file` user expectations.

```
# MEMORY.md example content
prefer cargo over rustc directly
homebrew package manager on this mac
postgres runs on port 5432, credentials in ~/.pgpass
```

No frontmatter, no headers, no usage indicators. Anything decorative
that hermes-agent adds (the `═════` separator, the
`[45% — 990/2200 chars]` percentage) is intentionally omitted — it
has no meaning without a character limit.

### Concurrency

Two distinct writers may target the same `MEMORY.md` (e.g. a CLI
session and a Telegram gateway session, both pointing at the same
profile). The store uses a `flock`-style exclusive file lock for
read-modify-write and `tempfile + std::fs::rename` for atomic replace.
This matches hermes-agent's pattern and prevents both interleaved
writes and the "truncate-then-write" race that plain `fs::write` has.

The lock file lives at `{path}.lock` and is acquired by every mutator.
Readers (`entries()`) do not need a lock because the rename pattern
guarantees readers see either the complete old file or the complete
new file. `load()` does not need a lock either (one-shot read at
startup, the directory is created if missing).

### Mutator semantics

- `add`:
  - Reject empty / whitespace-only `content`.
  - Skip exact duplicates (return success with a "no duplicate added"
    message).
  - Append to live state, write to disk under file lock.
- `replace`:
  - Find all entries containing `old` as a substring.
  - Zero matches → `NoMatch`.
  - Multiple matches with **different** bodies → `AmbiguousMatch` (do
    not mutate).
  - Multiple matches with **identical** bodies → operate on the
    first one (safe: deduplication upstream guarantees there is one
    logical entry).
  - One match → replace in place.
- `remove`:
  - Same matching rules as `replace`, but with no `new` argument.
- `read`:
  - Return all current entries for the target, no mutation.

All mutators return a `MemoryOpResult { target, entries, entry_count, message? }`
so the model can confirm the operation and observe the new state
without an extra `read` call.

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
        // Parse action + target from args.
        // Dispatch to MemoryStore::add / replace / remove / read.
        // Serialize the result (success or error) as JSON for the
        // model.
    }
}
```

### Schema (ported from hermes-agent)

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

`content` is required for `add` and `replace`; `old_text` is required
for `replace` and `remove`. Validation lives in the tool, not in
`MemoryStore` mutators — `MemoryStore::replace` takes both arguments
and the tool ensures both are present.

### Tool description (adapted)

Preserve the behavioral guidance from hermes-agent's `MEMORY_SCHEMA`
description: WHEN TO SAVE (corrections, preferences, environment
facts, conventions, stable facts), priority (user > environment >
procedural), TWO TARGETS (memory vs user), ACTIONS (add/replace/remove/read),
SKIP (trivial, raw dumps, temporary task state). Edit only the
language to fit perry_hermes's voice; the contract stays.

### Tool result format

Success: `{"success": true, "target": "memory", "entries": [...], "entry_count": 3, "message": "Entry added."}`
Error:   `{"success": false, "error": "no entry matched 'foo'"}`

The shape is stable so callers (and tests) can pattern-match.

## Component 5: `MemoryBlock` (`hermes-skill-tools::tools::memory::block`)

```rust
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

`MemoryBlock` does not lock or hit disk — it reads from the in-memory
`state`. Since the agent calls `build_system_message` once per session
(at `AgentLoop::new_session`), and the result is frozen in
`AgentSession.system_message`, the rendered block is effectively
immutable for the session's lifetime.

## Component 6: `AgentsMdBlock` (`hermes-agent::prompting`)

```rust
pub struct AgentsMdBlock {
    working_dir: PathBuf,
}

impl AgentsMdBlock {
    pub fn new(working_dir: PathBuf) -> Self { Self { working_dir } }
}

#[async_trait]
impl PromptContextBlock for AgentsMdBlock {
    fn name(&self) -> &str { "AGENTS.md" }
    async fn load(&self) -> Option<String> {
        // Existing load_agents_md_block, called from a sync helper
        // wrapped in a non-async block (file is small, no contention).
        load_agents_md_block(&self.working_dir)
    }
}
```

`load_agents_md_block` stays as a sync helper because the existing
implementation is `std::fs::read_to_string`, no async work needed.
The trait method is async to keep the interface uniform.

## Component 7: `build_system_message` (modified)

```rust
// crates/hermes-agent/src/prompting.rs

pub async fn build_system_message(
    working_dir: &Path,
    blocks: &[Arc<dyn PromptContextBlock>],
) -> Option<Message> {
    let mut sections: Vec<String> = Vec::with_capacity(blocks.len() + 2);
    if let Some(base) = compose_session_prompt_prefix() {
        sections.push(base.trim().to_string());
    }
    for block in blocks {
        match block.load().await {
            Some(body) => sections.push(format!("{}\n\n{}", block.name(), body)),
            None => {}
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

### Block order at the call site

The `AgentLoop` constructor accepts `blocks: Vec<Arc<dyn PromptContextBlock>>`
and the caller (CLI / TUI / gateway) supplies them in this order:

1. `AgentsMdBlock::new(working_dir)` — project context
2. `MemoryBlock::memory(store)` — agent's own notes
3. `MemoryBlock::user(store)` — user profile

This order puts project context closest to the base prompt and user
context last among the block list. Working-directory hint comes after
all blocks (it is appended unconditionally by `build_system_message`).

## Configuration

No `[memory]` TOML block. The tool is registered by default; users
disable via the existing `disabled_toolsets` list:

```toml
[agent]
disabled_toolsets = ["memory"]
```

The `memories_dir` is resolved at runtime from the same rules as
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
| `MemoryStore` mutator called with invalid target | `MemoryError::InvalidTarget` |
| `add` with empty content | `MemoryError::MissingContent` |
| `replace` / `remove` with no match | `MemoryError::NoMatch` |
| `replace` / `remove` with ambiguous match | `MemoryError::AmbiguousMatch` |
| File lock contention (e.g. sister session) | block on flock; no timeout |
| Disk write failure | `MemoryError::Io`, surfaced to model |
| `PromptContextBlock::load` error | logged via `tracing::warn!`, treated as `None` |
| `MemoryTool` invoked with missing required field | `ToolError::InvalidArgs` |

The model never sees an unwrapped IO error; it gets the JSON
`{success: false, error: "..."}` shape and can retry or change
approach.

## Testing

### `MemoryStore` (`hermes-skill-tools`)

- `load` with missing `MEMORY.md` returns empty store
- `load` with missing `USER.md` returns empty store
- `load` with both files populates `entries()` correctly
- `add` appends, returns success with new entry in `entries()`
- `add` with exact duplicate returns success with "no duplicate" message
- `add` with empty content returns `MissingContent`
- `replace` with one match substitutes in place
- `replace` with zero matches returns `NoMatch`
- `replace` with multiple **different** matches returns `AmbiguousMatch`
- `replace` with multiple **identical** matches (after dedup) replaces first
- `remove` with one match deletes
- `remove` with no match returns `NoMatch`
- `entries()` after mutator reflects new state
- Two `Arc<MemoryStore>` instances loaded from the same dir, both
  calling `add` concurrently → final state contains both entries
  (concurrency / flock test, gated on Unix)
- After `add`, the on-disk file uses the `§` delimiter and round-trips
  through `load`

### `MemoryTool` (`hermes-skill-tools`)

- All four actions dispatch correctly
- Success responses are JSON `{success: true, target, entries, entry_count, message?}`
- Error responses are JSON `{success: false, error: "..."}`
- Missing `action` returns `ToolError::InvalidArgs`
- Missing required field per action returns `ToolError::InvalidArgs`

### `MemoryBlock` (`hermes-skill-tools`)

- `name()` returns "MEMORY" or "USER" per constructor
- `load()` returns `None` when store is empty
- `load()` returns `"text1\n\ntext2"` style joined body for non-empty store

### `AgentsMdBlock` (`hermes-agent`)

- `name()` returns "AGENTS.md"
- `load()` returns `None` when file missing
- `load()` returns `"AGENTS.md\n\n{body}"` when file present (existing
  `load_agents_md_block` is the body producer; the wrapper adds the
  label)

### `build_system_message` (`hermes-agent`)

The existing test functions (`build_system_message_includes_working_dir_even_without_agents`,
`build_system_message_includes_default_base_prompt_and_working_dir`,
`build_system_message_orders_base_agents_md_working_dir`,
`build_system_message_omits_agents_block_when_file_missing`,
`build_system_message_reads_agents_md_from_session_working_dir_not_process_cwd`)
must be converted from `#[test]` to `#[tokio::test]` and `.await`
the new async function. Their assertions on the working-dir-only and
AGENTS.md-only behavior remain valid as long as the blocks list is
empty.

- Empty blocks list returns the base prompt + working-dir hint only
- Block order in `sections` matches the order in the input slice
- Block returning `None` is silently skipped
- Working-directory hint is always the last section
- The result is identical to the pre-existing test cases when the
  blocks list is empty (preserves existing behavior)

## File-by-file change summary

| File | Status | Notes |
|------|--------|-------|
| `crates/hermes-core/src/prompt_context.rs` | new | trait only |
| `crates/hermes-core/src/memory.rs` | new | `MemoryTarget`, `MemoryError` |
| `crates/hermes-core/src/lib.rs` | edit | add `prompt_context`, `memory` modules |
| `crates/hermes-skill-tools/src/tools/memory/mod.rs` | new | re-exports |
| `crates/hermes-skill-tools/src/tools/memory/store.rs` | new | `MemoryStore`, `MemoryConfig`, `MemoryOpResult`, `MemoryReadResult` |
| `crates/hermes-skill-tools/src/tools/memory/tool.rs` | new | `MemoryTool` |
| `crates/hermes-skill-tools/src/tools/memory/block.rs` | new | `MemoryBlock` |
| `crates/hermes-skill-tools/src/tools/mod.rs` | edit | add `pub mod memory;` |
| `crates/hermes-skill-tools/Cargo.toml` | edit | add `fs2` for cross-platform flock, `tempfile` |
| `crates/hermes-agent/src/prompting.rs` | edit | `AgentsMdBlock`, async `build_system_message(blocks)`, `resolve_memories_dir` |
| `crates/hermes-agent/src/tool_catalog.rs` | edit | `build_registry` accepts `Arc<MemoryStore>`, registers `MemoryTool` |
| `crates/hermes-agent/src/loop_engine/agent_loop.rs` | edit | `AgentLoop` holds `blocks: Vec<Arc<dyn PromptContextBlock>>`, passes them to `build_system_message` |
| `crates/hermes-agent/src/loop_engine/run.rs` (if applicable) | edit | same `build_system_message` call site updates |
| CLI / TUI / gateway binaries | edit | construct blocks list, pass to `AgentLoop::new` |

## Out of Scope (Future Work)

These are intentionally NOT part of this design. Listed so future
implementations know they were considered.

- Memory provider plugin system (`MemoryProvider` trait, external
  backends). perry_hermes has no plugin system today; building one
  for memory alone is premature.
- Auto-summarization / extraction on session end.
- Per-user / per-profile isolation beyond what `$PERRY_HERMES_HOME`
  already provides.
- Embedding-based semantic recall.
- Drift detection (would require a separate audit pass over the file
  format; not needed while only the `memory` tool writes the file).
- Capacity / character limits.
- `[memory]` TOML configuration block.
