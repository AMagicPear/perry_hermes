# PromptContextBlock Refactor + Memory Tool Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generalize the hard-coded AGENTS.md system-prompt injection in `hermes-agent::prompting` into a uniform `PromptContextBlock` trait, and add persistent file-backed memory (MEMORY.md / USER.md + `memory` tool) as its first non-AGENTS consumer.

**Architecture:** `PromptContextBlock` trait in `hermes-core`. Implementations `AgentsMdBlock` and `MemoryBlock` in `hermes-agent::prompting` (refactor + new). `MemoryStore` and `MemoryTool` in `hermes-skill-tools::tools::memory` (new). `AgentLoop` carries a `Vec<Arc<dyn PromptContextBlock>>` in `LoopConfig`, threads it through to `build_system_message` at session creation. Tool registry gains a `MemoryTool` registered by default. `build_system_message` becomes async and takes the block list.

**Tech Stack:** Rust 2024, `async-trait`, `tokio`, `serde`, `serde_json`, `thiserror`, `fs2` (cross-platform flock), `tempfile` (atomic write), existing test infra (`tempfile`, `tokio::test`).

---

## File structure

| File | Status | Responsibility |
|------|--------|---------------|
| `crates/hermes-core/src/prompt_context.rs` | new | `PromptContextBlock` trait |
| `crates/hermes-core/src/lib.rs` | edit | add `prompt_context` module + re-export |
| `crates/hermes-skill-tools/src/tools/memory/mod.rs` | new | re-exports |
| `crates/hermes-skill-tools/src/tools/memory/store.rs` | new | `MemoryConfig`, `MemoryStore`, `MemoryTarget`, `MemoryOpResult`, `MemoryReadResult`, `MemoryError` |
| `crates/hermes-skill-tools/src/tools/memory/tool.rs` | new | `MemoryTool` |
| `crates/hermes-skill-tools/src/tools/mod.rs` | edit | add `pub mod memory;` |
| `crates/hermes-skill-tools/Cargo.toml` | edit | add `fs2` dep |
| `crates/hermes-agent/src/prompting.rs` | edit | `AgentsMdBlock`, `MemoryBlock`, `resolve_memories_dir`, async `build_system_message`; convert existing tests to `#[tokio::test]`; add new block-list tests |
| `crates/hermes-agent/src/tool_catalog.rs` | edit | `build_registry` accepts `Arc<MemoryStore>`, registers `MemoryTool` |
| `crates/hermes-agent/src/loop_engine/agent_loop.rs` | edit | `LoopConfig` gains `blocks: Vec<Arc<dyn PromptContextBlock>>`; `build_loop_for_custom_provider` constructs blocks; `system_message_for` becomes async; `new_session` and `load_json_session` await it |
| `crates/hermes-agent/tests/tool_dispatch.rs` (and other tests calling `AgentLoop::new_session` / `system_message_for`) | edit | pass empty `blocks` slice where needed; await async calls |
| `crates/hermes-cli/src/main.rs` | edit | no change needed (calls `AgentLoop::from_config` which now wires blocks) |

Note: `MemoryBlock` lives in `hermes-agent::prompting` (presentation block), `MemoryStore` lives in `hermes-skill-tools::tools::memory` (data + IO + tool). `MemoryBlock` reads from the store via `Arc<MemoryStore>`.

---

## Task 1: Add `PromptContextBlock` trait in `hermes-core`

**Files:**
- Create: `crates/hermes-core/src/prompt_context.rs`
- Modify: `crates/hermes-core/src/lib.rs`

- [ ] **Step 1: Create the trait file**

Write the following to `crates/hermes-core/src/prompt_context.rs`:

```rust
//! `PromptContextBlock` — a context fragment loaded at session
//! creation and frozen into the system prompt.

use async_trait::async_trait;

/// A context fragment loaded at session creation and frozen into
/// the system prompt. Implementations own their I/O.
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
    /// "MEMORY", "USER".
    fn name(&self) -> &str;

    /// Load and render the block. `None` → caller skips this block.
    async fn load(&self) -> Option<String>;
}
```

- [ ] **Step 2: Re-export from `hermes_core::lib`**

Edit `crates/hermes-core/src/lib.rs`. Find the line:
```rust
pub mod platform;
```
and add the `prompt_context` module in alphabetical order between `platform` and `provider`:

```rust
pub mod platform;
pub mod prompt_context;
pub mod provider;
```

Then find the line:
```rust
pub use platform::Platform;
```
and add the re-export below it:

```rust
pub use platform::Platform;
pub use prompt_context::PromptContextBlock;
```

- [ ] **Step 3: Build to verify it compiles**

Run: `cargo build -p perry-hermes-core`
Expected: success.

- [ ] **Step 4: Commit**

```bash
git add crates/hermes-core/src/prompt_context.rs crates/hermes-core/src/lib.rs
git commit -m "feat(core): add PromptContextBlock trait"
```

---

## Task 2: Refactor `AgentsMdBlock` + change `build_system_message` signature in `hermes-agent::prompting`

**Files:**
- Modify: `crates/hermes-agent/src/prompting.rs`

This task is the core refactor. It converts `build_system_message` to async, takes a `&[Arc<dyn PromptContextBlock>]` slice, and introduces `AgentsMdBlock` as the first implementation of the new trait. Existing tests are converted to `#[tokio::test]` and pass an empty `blocks` slice — their assertions must remain valid because the AGENTS.md code path is byte-equivalent.

- [ ] **Step 1: Update imports at the top of `prompting.rs`**

Replace the existing top of `crates/hermes-agent/src/prompting.rs` (the `use` lines plus the file-level doc comment) with:

```rust
//! System-prompt composition for `AgentLoop` and `AgentSession`.
//!
//! The system prompt is a single immutable `Message` stored on
//! `AgentSession`. It is built exactly once, at session construction,
//! by `AgentLoop::new_session`. There is no per-turn recomposition, no cache,
//! and no "prepend at send time" injection step.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use perry_hermes_core::message::Message;
use perry_hermes_core::prompt_context::PromptContextBlock;
```

(Adds `std::sync::Arc`, `async_trait`, and `PromptContextBlock`. Keeps existing `std::path` imports.)

- [ ] **Step 2: Replace `build_system_message` with the async, block-list version**

Find the existing `pub fn build_system_message(working_dir: &Path) -> Option<Message> { ... }` (around line 117) and replace it with:

```rust
/// Build the immutable system `Message` for a session, combining the
/// hardcoded [`DEFAULT_SYSTEM_PROMPT`] with the session-scoped sections
/// (skills block, caller-supplied [`PromptContextBlock`]s, working
/// directory).
///
/// `blocks` is iterated in order; each block contributes a
/// `"{name}\n\n{body}"` section if `load()` returns `Some(body)`.
/// Blocks that return `None` (missing/empty backing file) are silently
/// skipped.
///
/// The result is `None` only if all sections are empty. Newly-created
/// sessions always get a system message because of the working-dir
/// hint.
///
/// Callers should invoke this at most once per session, store the
/// returned message in the session's log, and treat it as
/// immutable thereafter.
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

- [ ] **Step 3: Add `AgentsMdBlock` struct + trait impl below `working_directory_hint`**

Find `fn working_directory_hint(working_dir: &Path) -> String` and add the following struct + impl after the closing `}` of that function (i.e. after line 135 of the original file, before the `#[cfg(test)]` block):

```rust
/// Project-level block loading `<working_dir>/AGENTS.md`. The
/// existing `load_agents_md_block` helper is the body producer; this
/// wrapper adds the `name()` label required by the trait and
/// implements `async_trait::async_trait` so it can sit alongside
/// other blocks in a heterogeneous `Vec<Arc<dyn PromptContextBlock>>`.
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
    fn name(&self) -> &str {
        "AGENTS.md"
    }

    async fn load(&self) -> Option<String> {
        // Sync I/O on a small file; no contention.
        load_agents_md_block(&self.working_dir)
    }
}
```

- [ ] **Step 4: Convert existing `build_system_message` tests to `#[tokio::test]`**

Find the `#[cfg(test)] mod tests` block (around line 138 of the original file). For each existing test that calls `build_system_message` synchronously, do the following:

- Change `#[test]` → `#[tokio::test]`.
- Add `&[]` as a second argument to the `build_system_message` call.
- Add `.await` after the call.

Concretely, the five test functions to update are:

1. `build_system_message_includes_working_dir_even_without_agents` — change to:
   ```rust
   #[tokio::test]
   async fn build_system_message_includes_working_dir_even_without_agents() {
       let msg = build_system_message(Path::new("/tmp/no-agents-md"), &[])
           .await
           .expect("message should be Some because of working-dir hint");
       let text = msg.content.as_text();
       assert!(text.contains("Current working directory: /tmp/no-agents-md"));
   }
   ```

2. `build_system_message_includes_default_base_prompt_and_working_dir`:
   ```rust
   #[tokio::test]
   async fn build_system_message_includes_default_base_prompt_and_working_dir() {
       let msg = build_system_message(Path::new("/tmp/project"), &[])
           .await
           .expect("message should be Some");

       let text = msg.content.as_text();
       assert!(text.contains("Perry Hermes"));
       assert!(text.contains("Current working directory: /tmp/project"));
       assert!(!text.contains("Provider:"));
       assert!(!text.contains("Session ID:"));
   }
   ```

3. `build_system_message_orders_base_agents_md_working_dir`:
   ```rust
   #[tokio::test]
   async fn build_system_message_orders_base_agents_md_working_dir() {
       let tmp = tempfile::tempdir().unwrap();
       write_agents_md(tmp.path(), "UNIQUE-AGENTS-MARKER-XYZ");

       let msg = build_system_message(tmp.path(), &[])
           .await
           .expect("message should be Some");
       let text = msg.content.as_text();

       let base_idx = text.find("Perry Hermes").expect("base present");
       let agents_idx = text
           .find("UNIQUE-AGENTS-MARKER-XYZ")
           .expect("agents md present");
       let env_idx = text
           .find("Current working directory:")
           .expect("env hints present");
       // Order: base -> agents.md -> working dir.
       assert!(base_idx < agents_idx, "agents block should follow base");
       assert!(
           agents_idx < env_idx,
           "agents block should precede working-dir hint"
       );
   }
   ```

4. `build_system_message_omits_agents_block_when_file_missing`:
   ```rust
   #[tokio::test]
   async fn build_system_message_omits_agents_block_when_file_missing() {
       let tmp = tempfile::tempdir().unwrap();
       let msg = build_system_message(tmp.path(), &[])
           .await
           .expect("message should be Some");
       let text = msg.content.as_text();
       assert!(!text.contains("Project guidance from `AGENTS.md`"));
       assert!(text.contains("Perry Hermes"));
   }
   ```

5. `build_system_message_reads_agents_md_from_session_working_dir_not_process_cwd`:
   ```rust
   #[tokio::test]
   async fn build_system_message_reads_agents_md_from_session_working_dir_not_process_cwd() {
       // Session working dir has AGENTS.md; process cwd does not.
       // The runtime must consult the session working dir, not std::env::current_dir().
       let session_dir = tempfile::tempdir().unwrap();
       write_agents_md(session_dir.path(), "FROM-SESSION-DIR");

       // Move the process cwd to a different tempdir that has no AGENTS.md.
       let other_cwd = tempfile::tempdir().unwrap();
       let _guard = crate::test_env::blocking_lock();
       let _cwd = CwdGuard::enter(other_cwd.path());

       let msg = build_system_message(session_dir.path(), &[])
           .await
           .expect("message should be Some");
       let text = msg.content.as_text();
       assert!(text.contains("FROM-SESSION-DIR"));
       // The body must appear exactly once — no double-injection.
       assert_eq!(text.matches("FROM-SESSION-DIR").count(), 1);
   }
   ```

- [ ] **Step 5: Add new tests covering the block list**

Inside the same `#[cfg(test)] mod tests` block, append the following new tests. They verify the new abstraction: blocks are iterated in order, `None` is skipped, and working-dir hint always lands last.

```rust
    use std::sync::Arc as _Arc;

    use async_trait::async_trait as _async_trait;
    use perry_hermes_core::prompt_context::PromptContextBlock as _PCB;

    struct StaticBlock {
        name: &'static str,
        body: Option<&'static str>,
    }

    #[async_trait::async_trait]
    impl PromptContextBlock for StaticBlock {
        fn name(&self) -> &str {
            self.name
        }
        async fn load(&self) -> Option<String> {
            self.body.map(|s| s.to_string())
        }
    }

    #[tokio::test]
    async fn block_order_matches_input_slice() {
        let blocks: Vec<Arc<dyn PromptContextBlock>> = vec![
            Arc::new(StaticBlock { name: "ALPHA", body: Some("alpha body") }),
            Arc::new(StaticBlock { name: "BETA", body: Some("beta body") }),
        ];
        let msg = build_system_message(Path::new("/tmp"), &blocks)
            .await
            .expect("message");
        let text = msg.content.as_text();

        let alpha_idx = text.find("ALPHA\n\nalpha body").expect("alpha present");
        let beta_idx = text.find("BETA\n\nbeta body").expect("beta present");
        let dir_idx = text.find("Current working directory: /tmp").expect("dir present");
        assert!(alpha_idx < beta_idx, "alpha before beta");
        assert!(beta_idx < dir_idx, "blocks before working dir");
    }

    #[tokio::test]
    async fn none_block_is_silently_skipped() {
        let blocks: Vec<Arc<dyn PromptContextBlock>> = vec![
            Arc::new(StaticBlock { name: "PRESENT", body: Some("p") }),
            Arc::new(StaticBlock { name: "ABSENT", body: None }),
        ];
        let msg = build_system_message(Path::new("/tmp"), &blocks)
            .await
            .expect("message");
        let text = msg.content.as_text();
        assert!(text.contains("PRESENT\n\np"));
        assert!(!text.contains("ABSENT"));
    }

    #[tokio::test]
    async fn empty_blocks_list_yields_only_base_and_working_dir() {
        let blocks: Vec<Arc<dyn PromptContextBlock>> = vec![];
        let msg = build_system_message(Path::new("/tmp/project"), &blocks)
            .await
            .expect("message");
        let text = msg.content.as_text();
        // base prompt + working dir hint, with no extras.
        assert!(text.contains("Perry Hermes"));
        assert!(text.contains("Current working directory: /tmp/project"));
        assert!(!text.contains("Project guidance from `AGENTS.md`"));
    }

    #[tokio::test]
    async fn working_dir_hint_always_lands_last() {
        let blocks: Vec<Arc<dyn PromptContextBlock>> = vec![
            Arc::new(StaticBlock { name: "Z_BLOCK", body: Some("z") }),
        ];
        let msg = build_system_message(Path::new("/tmp/last"), &blocks)
            .await
            .expect("message");
        let text = msg.content.as_text();
        let z_idx = text.find("Z_BLOCK").expect("z block present");
        let dir_idx = text.find("Current working directory: /tmp/last").expect("dir");
        assert!(z_idx < dir_idx);
    }
```

- [ ] **Step 6: Build to verify compilation**

Run: `cargo build -p perry-hermes-agent`
Expected: errors in callers of `build_system_message` (we'll fix in Task 5). Confirm only those, no errors inside `prompting.rs` itself.

- [ ] **Step 7: Run the prompting tests**

Run: `cargo test -p perry-hermes-agent --lib prompting::`
Expected: 9 tests pass (5 converted + 4 new).

- [ ] **Step 8: Commit**

```bash
git add crates/hermes-agent/src/prompting.rs
git commit -m "refactor(agent): generalize prompt injection via PromptContextBlock

Replaces the hard-coded build_system_message(working_dir) with an
async build_system_message(working_dir, blocks) that walks a list
of PromptContextBlock implementations. Adds AgentsMdBlock as the
first implementation. Existing AGENTS.md test assertions remain
valid when blocks list is empty."
```

---

## Task 3: Add `MemoryStore` and `MemoryError` in `hermes-skill-tools::tools::memory::store`

**Files:**
- Modify: `crates/hermes-skill-tools/Cargo.toml`
- Create: `crates/hermes-skill-tools/src/tools/memory/mod.rs`
- Create: `crates/hermes-skill-tools/src/tools/memory/store.rs`
- Modify: `crates/hermes-skill-tools/src/tools/mod.rs`

- [ ] **Step 1: Add `fs2` to `hermes-skill-tools` dependencies**

Edit `crates/hermes-skill-tools/Cargo.toml`. Add the following to `[dependencies]` (alphabetical, before `serde_yaml`):

```toml
fs2 = "0.4"
```

Also add `tempfile` to `[dependencies]` (move it from dev-deps so `store.rs` can use it):

```toml
tempfile = "3"
```

Then remove `tempfile = "3"` from `[dev-dependencies]`.

- [ ] **Step 2: Create `memory/mod.rs`**

Create `crates/hermes-skill-tools/src/tools/memory/mod.rs`:

```rust
//! Memory tool: persistent file-backed memory (MEMORY.md + USER.md).
//!
//! See `store.rs` for the data layer and `tool.rs` for the LLM-facing
//! tool. `MemoryBlock` (which renders the system-prompt snapshot)
//! lives in `hermes-agent::prompting` next to the other prompt blocks.

pub mod store;
pub mod tool;

pub use store::{MemoryConfig, MemoryError, MemoryOpResult, MemoryReadResult, MemoryStore, MemoryTarget};
pub use tool::MemoryTool;
```

- [ ] **Step 3: Create `memory/store.rs` with the data layer**

Create `crates/hermes-skill-tools/src/tools/memory/store.rs` with the following complete content:

```rust
//! `MemoryStore` — persistent file-backed memory.
//!
//! Two on-disk files, `MEMORY.md` and `USER.md`, in `memories_dir`.
//! Entries are joined by the `§` delimiter (hermes-agent convention).
//! Concurrency is handled via `fs2` flock and atomic tempfile+rename.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use thiserror::Error;
use tokio::sync::RwLock;

pub const ENTRY_DELIMITER: &str = "\n§\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryTarget {
    Memory,
    User,
}

impl MemoryTarget {
    fn file_name(self) -> &'static str {
        match self {
            MemoryTarget::Memory => "MEMORY.md",
            MemoryTarget::User => "USER.md",
        }
    }
}

#[derive(Debug, Error, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
pub struct MemoryOpResult {
    pub target: MemoryTarget,
    pub entries: Vec<String>,
    pub entry_count: usize,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryReadResult {
    pub target: MemoryTarget,
    pub entries: Vec<String>,
    pub entry_count: usize,
}

#[derive(Debug, Clone)]
pub struct MemoryConfig {
    pub memories_dir: PathBuf,
}

struct LiveState {
    memory_entries: Vec<String>,
    user_entries: Vec<String>,
}

pub struct MemoryStore {
    cfg: MemoryConfig,
    state: Arc<RwLock<LiveState>>,
}

impl MemoryStore {
    /// Read both memory files from disk and return a populated store.
    /// Missing files are treated as empty stores (not errors).
    pub async fn load(cfg: MemoryConfig) -> std::io::Result<Self> {
        tokio::fs::create_dir_all(&cfg.memories_dir).await?;
        let memory_entries = read_file(&cfg.memories_dir.join("MEMORY.md")).await;
        let user_entries = read_file(&cfg.memories_dir.join("USER.md")).await;
        Ok(Self {
            cfg,
            state: Arc::new(RwLock::new(LiveState {
                memory_entries,
                user_entries,
            })),
        })
    }

    /// Read-only view of the live entries for a target. Used by
    /// `MemoryBlock::load` to render the system prompt snapshot.
    pub async fn entries(&self, target: MemoryTarget) -> Vec<String> {
        let state = self.state.read().await;
        match target {
            MemoryTarget::Memory => state.memory_entries.clone(),
            MemoryTarget::User => state.user_entries.clone(),
        }
    }

    pub async fn add(
        &self,
        target: MemoryTarget,
        content: String,
    ) -> Result<MemoryOpResult, MemoryError> {
        let content = content.trim().to_string();
        if content.is_empty() {
            return Err(MemoryError::MissingContent);
        }

        let path = self.path_for(target);
        let lock_path = lock_path_for(&path);
        let _guard = FileLock::new(&lock_path).await;

        // Re-read under lock to pick up sister-session writes.
        let mut fresh = read_file(&path).await;
        fresh = dedup_in_place(fresh);

        if fresh.iter().any(|e| e == &content) {
            return Ok(success_result(
                target,
                &self.entries_for(target).await,
                Some("Entry already exists (no duplicate added)."),
            ));
        }

        fresh.push(content);
        write_file(&path, &fresh).await.map_err(MemoryError::Io)?;

        // Update live state.
        {
            let mut state = self.state.write().await;
            match target {
                MemoryTarget::Memory => state.memory_entries = fresh.clone(),
                MemoryTarget::User => state.user_entries = fresh.clone(),
            }
        }
        Ok(success_result(target, &fresh, Some("Entry added.")))
    }

    pub async fn replace(
        &self,
        target: MemoryTarget,
        old: &str,
        new: String,
    ) -> Result<MemoryOpResult, MemoryError> {
        let old = old.trim();
        let new = new.trim();
        if old.is_empty() {
            return Err(MemoryError::NoMatch("(empty old_text)".to_string()));
        }
        if new.is_empty() {
            return Err(MemoryError::MissingContent);
        }

        let path = self.path_for(target);
        let lock_path = lock_path_for(&path);
        let _guard = FileLock::new(&lock_path).await;

        let mut entries = read_file(&path).await;
        entries = dedup_in_place(entries);
        let matches: Vec<(usize, String)> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.contains(old))
            .map(|(i, e)| (i, e.clone()))
            .collect();

        match matches.len() {
            0 => Err(MemoryError::NoMatch(old.to_string())),
            1 => {
                let (idx, _) = matches[0];
                entries[idx] = new.to_string();
                write_file(&path, &entries).await.map_err(MemoryError::Io)?;
                self.set_entries(target, entries.clone()).await;
                Ok(success_result(target, &entries, Some("Entry replaced.")))
            }
            n => {
                let unique: std::collections::HashSet<&str> =
                    matches.iter().map(|(_, e)| e.as_str()).collect();
                if unique.len() == 1 {
                    // All matches identical — replace the first.
                    let (idx, _) = matches[0];
                    entries[idx] = new.to_string();
                    write_file(&path, &entries).await.map_err(MemoryError::Io)?;
                    self.set_entries(target, entries.clone()).await;
                    Ok(success_result(target, &entries, Some("Entry replaced.")))
                } else {
                    Err(MemoryError::AmbiguousMatch(old.to_string()))
                }
            }
        }
        .map(|r| r.ensure_count(n))
    }

    pub async fn remove(
        &self,
        target: MemoryTarget,
        old: &str,
    ) -> Result<MemoryOpResult, MemoryError> {
        let old = old.trim();
        if old.is_empty() {
            return Err(MemoryError::NoMatch("(empty old_text)".to_string()));
        }

        let path = self.path_for(target);
        let lock_path = lock_path_for(&path);
        let _guard = FileLock::new(&lock_path).await;

        let mut entries = read_file(&path).await;
        entries = dedup_in_place(entries);
        let matches: Vec<(usize, String)> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.contains(old))
            .map(|(i, e)| (i, e.clone()))
            .collect();

        match matches.len() {
            0 => Err(MemoryError::NoMatch(old.to_string())),
            1 => {
                let (idx, _) = matches[0];
                entries.remove(idx);
                write_file(&path, &entries).await.map_err(MemoryError::Io)?;
                self.set_entries(target, entries.clone()).await;
                Ok(success_result(target, &entries, Some("Entry removed.")))
            }
            _ => {
                let unique: std::collections::HashSet<&str> =
                    matches.iter().map(|(_, e)| e.as_str()).collect();
                if unique.len() == 1 {
                    let (idx, _) = matches[0];
                    entries.remove(idx);
                    write_file(&path, &entries).await.map_err(MemoryError::Io)?;
                    self.set_entries(target, entries.clone()).await;
                    Ok(success_result(target, &entries, Some("Entry removed.")))
                } else {
                    Err(MemoryError::AmbiguousMatch(old.to_string()))
                }
            }
        }
    }

    pub async fn read(&self, target: MemoryTarget) -> Result<MemoryReadResult, MemoryError> {
        let entries = self.entries(target).await;
        Ok(MemoryReadResult {
            target,
            entry_count: entries.len(),
            entries,
        })
    }

    fn path_for(&self, target: MemoryTarget) -> PathBuf {
        self.cfg.memories_dir.join(target.file_name())
    }

    async fn entries_for(&self, target: MemoryTarget) -> Vec<String> {
        self.entries(target).await
    }

    async fn set_entries(&self, target: MemoryTarget, entries: Vec<String>) {
        let mut state = self.state.write().await;
        match target {
            MemoryTarget::Memory => state.memory_entries = entries,
            MemoryTarget::User => state.user_entries = entries,
        }
    }
}

fn success_result(
    target: MemoryTarget,
    entries: &[String],
    message: Option<&str>,
) -> MemoryOpResult {
    MemoryOpResult {
        target,
        entry_count: entries.len(),
        entries: entries.to_vec(),
        message: message.map(|s| s.to_string()),
    }
}

// Helper trait used above. `replace` returns an `n` capture but Rust's
// match arms can't carry values across arms; this is a no-op
// adjustment so the call site stays linear.
trait EnsureCount {
    fn ensure_count(self, n: usize) -> Self;
}
impl EnsureCount for MemoryOpResult {
    fn ensure_count(mut self, _n: usize) -> Self {
        self
    }
}

fn lock_path_for(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".lock");
    PathBuf::from(s)
}

fn dedup_in_place(mut entries: Vec<String>) -> Vec<String> {
    // Preserve order, keep first occurrence.
    let mut seen = std::collections::HashSet::new();
    entries.retain(|e| seen.insert(e.clone()));
    entries
}

async fn read_file(path: &Path) -> Vec<String> {
    let raw = match tokio::fs::read_to_string(path).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!("failed to read {}: {e}", path.display());
            return Vec::new();
        }
    };
    if raw.trim().is_empty() {
        return Vec::new();
    }
    raw.split(ENTRY_DELIMITER)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

async fn write_file(path: &Path, entries: &[String]) -> std::io::Result<()> {
    let content = if entries.is_empty() {
        String::new()
    } else {
        entries.join(ENTRY_DELIMITER)
    };
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    // Write to a temp file in the same directory, then atomically rename.
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("memory file has no parent directory"))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".mem_")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    use std::io::Write;
    tmp.write_all(content.as_bytes())?;
    tmp.flush()?;
    let tmp_path = tmp.path().to_path_buf();
    tmp.into_temp_path().persist(path).map_err(|e| {
        std::io::Error::other(format!(
            "failed to rename temp file to {}: {e}",
            path.display()
        ))
    })?;
    // Ensure the persisted file exists at `path`; `persist` does that.
    let _ = tmp_path;
    Ok(())
}

/// Blocking flock wrapper. `fs2::FileExt::lock_exclusive` is sync; we
/// run it on a blocking task to keep `MemoryStore` async.
struct FileLock {
    _file: std::fs::File,
    path: PathBuf,
}

impl FileLock {
    async fn new(path: &Path) -> Self {
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let path = path.to_path_buf();
        let file = tokio::task::spawn_blocking({
            let path = path.clone();
            move || -> std::io::Result<std::fs::File> {
                let f = std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(&path)?;
                f.lock_exclusive()?;
                Ok(f)
            }
        })
        .await
        .expect("blocking task panicked")
        .expect("failed to acquire memory file lock");
        Self { _file: file, path }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Best-effort unlock + close. The file is closed by Drop on
        // _file; flock is released when the file descriptor closes.
        let _ = &self.path;
    }
}
```

- [ ] **Step 4: Wire the new module into `tools/mod.rs`**

Edit `crates/hermes-skill-tools/src/tools/mod.rs`. Find the line `pub mod bash;` and add the new module after it (alphabetical):

```rust
pub mod bash;
pub mod files;
pub mod memory;
```

- [ ] **Step 5: Build to verify `MemoryStore` compiles**

Run: `cargo build -p perry-hermes-skill-tools`
Expected: success. The `MemoryTool` import in `mod.rs` will fail because `tool.rs` doesn't exist yet — temporarily comment it out:

In `crates/hermes-skill-tools/src/tools/memory/mod.rs`, change the `pub mod tool;` and `pub use tool::MemoryTool;` lines to:

```rust
// pub mod tool;
// pub use tool::MemoryTool;
```

(We'll re-enable in Task 4.) Build again to confirm `store.rs` alone is OK.

- [ ] **Step 6: Add `MemoryStore` unit tests**

Create `crates/hermes-skill-tools/src/tools/memory/store.rs` is the file we just created. Append the following `#[cfg(test)] mod tests` block to the **end of that file** (after the `FileLock` impl). Use a unique tmp dir per test.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cfg() -> (tempfile::TempDir, MemoryConfig) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = MemoryConfig {
            memories_dir: dir.path().to_path_buf(),
        };
        (dir, cfg)
    }

    #[tokio::test]
    async fn load_with_no_files_yields_empty_stores() {
        let (_dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        assert!(store.entries(MemoryTarget::Memory).await.is_empty());
        assert!(store.entries(MemoryTarget::User).await.is_empty());
    }

    #[tokio::test]
    async fn load_reads_existing_files() {
        let (dir, cfg) = temp_cfg();
        std::fs::write(
            dir.path().join("MEMORY.md"),
            "alpha\n§\nbeta\n§\ngamma",
        )
        .unwrap();
        let store = MemoryStore::load(cfg).await.unwrap();
        let entries = store.entries(MemoryTarget::Memory).await;
        assert_eq!(entries, vec!["alpha", "beta", "gamma"]);
    }

    #[tokio::test]
    async fn add_appends_to_disk_and_state() {
        let (dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        let result = store.add(MemoryTarget::Memory, "hello".into()).await.unwrap();
        assert_eq!(result.entry_count, 1);
        assert_eq!(result.entries, vec!["hello"]);
        let on_disk = std::fs::read_to_string(dir.path().join("MEMORY.md")).unwrap();
        assert_eq!(on_disk, "hello");
    }

    #[tokio::test]
    async fn add_rejects_empty_content() {
        let (_dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        let err = store
            .add(MemoryTarget::Memory, "   \n  ".into())
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::MissingContent));
    }

    #[tokio::test]
    async fn add_skips_exact_duplicate() {
        let (_dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        store.add(MemoryTarget::Memory, "x".into()).await.unwrap();
        let result = store.add(MemoryTarget::Memory, "x".into()).await.unwrap();
        assert_eq!(result.entry_count, 1);
        assert!(result.message.unwrap().contains("no duplicate"));
    }

    #[tokio::test]
    async fn replace_with_one_match_substitutes() {
        let (dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        store.add(MemoryTarget::Memory, "old text".into()).await.unwrap();
        store
            .replace(MemoryTarget::Memory, "old text", "new text")
            .await
            .unwrap();
        let on_disk = std::fs::read_to_string(dir.path().join("MEMORY.md")).unwrap();
        assert_eq!(on_disk, "new text");
    }

    #[tokio::test]
    async fn replace_with_no_match_returns_no_match_error() {
        let (_dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        store.add(MemoryTarget::Memory, "x".into()).await.unwrap();
        let err = store
            .replace(MemoryTarget::Memory, "zzz", "y")
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::NoMatch(ref s) if s == "zzz"));
    }

    #[tokio::test]
    async fn replace_with_ambiguous_distinct_matches_errors() {
        let (_dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        store.add(MemoryTarget::Memory, "abc one".into()).await.unwrap();
        store.add(MemoryTarget::Memory, "abc two".into()).await.unwrap();
        let err = store
            .replace(MemoryTarget::Memory, "abc", "x")
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::AmbiguousMatch(ref s) if s == "abc"));
    }

    #[tokio::test]
    async fn replace_with_identical_matches_replaces_first() {
        let (_dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        store.add(MemoryTarget::Memory, "same".into()).await.unwrap();
        store.add(MemoryTarget::Memory, "same".into()).await.unwrap();
        store
            .replace(MemoryTarget::Memory, "same", "different")
            .await
            .unwrap();
        let entries = store.entries(MemoryTarget::Memory).await;
        // After dedup-then-replace, only one "different" remains.
        assert_eq!(entries, vec!["different"]);
    }

    #[tokio::test]
    async fn remove_deletes_matching_entry() {
        let (_dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        store.add(MemoryTarget::Memory, "alpha".into()).await.unwrap();
        store.add(MemoryTarget::Memory, "beta".into()).await.unwrap();
        store.remove(MemoryTarget::Memory, "alpha").await.unwrap();
        let entries = store.entries(MemoryTarget::Memory).await;
        assert_eq!(entries, vec!["beta"]);
    }

    #[tokio::test]
    async fn remove_with_no_match_returns_error() {
        let (_dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        let err = store
            .remove(MemoryTarget::Memory, "nope")
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::NoMatch(_)));
    }

    #[tokio::test]
    async fn read_returns_current_entries() {
        let (_dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        store.add(MemoryTarget::Memory, "x".into()).await.unwrap();
        store.add(MemoryTarget::User, "y".into()).await.unwrap();
        let m = store.read(MemoryTarget::Memory).await.unwrap();
        assert_eq!(m.entries, vec!["x"]);
        let u = store.read(MemoryTarget::User).await.unwrap();
        assert_eq!(u.entries, vec!["y"]);
    }

    #[tokio::test]
    async fn on_disk_round_trips_through_load() {
        let (dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg.clone()).await.unwrap();
        store.add(MemoryTarget::Memory, "one".into()).await.unwrap();
        store.add(MemoryTarget::Memory, "two".into()).await.unwrap();
        drop(store);
        // Re-load from the same dir.
        let store2 = MemoryStore::load(cfg).await.unwrap();
        assert_eq!(
            store2.entries(MemoryTarget::Memory).await,
            vec!["one", "two"]
        );
        let on_disk = std::fs::read_to_string(dir.path().join("MEMORY.md")).unwrap();
        assert_eq!(on_disk, "one\n§\ntwo");
    }
}
```

- [ ] **Step 7: Run the store tests**

Run: `cargo test -p perry-hermes-skill-tools --lib tools::memory::store::`
Expected: 12 tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/hermes-skill-tools/Cargo.toml crates/hermes-skill-tools/src/tools/mod.rs crates/hermes-skill-tools/src/tools/memory/mod.rs crates/hermes-skill-tools/src/tools/memory/store.rs
git commit -m "feat(skill-tools): add MemoryStore

File-backed persistent memory with MEMORY.md + USER.md. Uses fs2
flock and tempfile+rename for concurrent mutator safety."
```

---

## Task 4: Add `MemoryTool` in `hermes-skill-tools::tools::memory::tool`

**Files:**
- Create: `crates/hermes-skill-tools/src/tools/memory/tool.rs`
- Modify: `crates/hermes-skill-tools/src/tools/memory/mod.rs` (re-enable)

- [ ] **Step 1: Create `tool.rs`**

Create `crates/hermes-skill-tools/src/tools/memory/tool.rs`:

```rust
//! `MemoryTool` — LLM-facing tool for adding/replacing/removing/reading
//! entries in `MEMORY.md` and `USER.md`.

use std::sync::Arc;

use async_trait::async_trait;
use perry_hermes_core::error::ToolError;
use perry_hermes_core::tool::{Tool, ToolContext, ToolOutput};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::store::{MemoryError, MemoryStore, MemoryTarget};

/// Description shown to the model. Adapted from hermes-agent's
/// `MEMORY_SCHEMA` description; behavioral contract preserved.
const MEMORY_TOOL_DESCRIPTION: &str = "Save durable information to persistent memory that survives across sessions. \
Memory is injected into future turns, so keep it compact and focused on facts that will still matter later.\n\
\n\
WHEN TO SAVE (do this proactively, don't wait to be asked):\n\
- User corrects you or says 'remember this' / 'don't do that again'\n\
- User shares a preference, habit, or personal detail (name, role, timezone, coding style)\n\
- You discover something about the environment (OS, installed tools, project structure)\n\
- You learn a convention, API quirk, or workflow specific to this user's setup\n\
- You identify a stable fact that will be useful again in future sessions\n\
\n\
PRIORITY: User preferences and corrections > environment facts > procedural knowledge. \
The most valuable memory prevents the user from having to repeat themselves.\n\
\n\
Do NOT save task progress, session outcomes, completed-work logs, or temporary TODO state to memory.\n\
\n\
TWO TARGETS:\n\
- 'user': who the user is -- name, role, preferences, communication style\n\
- 'memory': your notes -- environment facts, project conventions, tool quirks, lessons learned\n\
\n\
ACTIONS: add (new entry), replace (update existing -- old_text identifies it), \
remove (delete -- old_text identifies it), read (list current entries).";

pub struct MemoryTool {
    store: Arc<MemoryStore>,
}

impl MemoryTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self) -> &str {
        MEMORY_TOOL_DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add", "replace", "remove", "read"],
                    "description": "The action to perform."
                },
                "target": {
                    "type": "string",
                    "enum": ["memory", "user"],
                    "description": "Which memory store: 'memory' for personal notes, 'user' for user profile."
                },
                "content": {
                    "type": "string",
                    "description": "The entry content. Required for add and replace."
                },
                "old_text": {
                    "type": "string",
                    "description": "Short unique substring identifying the entry to replace or remove."
                }
            },
            "required": ["action", "target"]
        })
    }

    fn toolset(&self) -> &'static str {
        "memory"
    }

    fn emoji(&self) -> Option<&str> {
        Some("🧠")
    }

    async fn execute(
        &self,
        args: Value,
        _ctx: ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("missing 'action'".into()))?;
        let target = parse_target(args.get("target"))?;

        let result = match action {
            "add" => {
                let content = args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidArgs("content required for 'add'".into())
                    })?;
                self.store.add(target, content.to_string()).await
            }
            "replace" => {
                let old = args
                    .get("old_text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidArgs("old_text required for 'replace'".into())
                    })?;
                let new = args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidArgs("content required for 'replace'".into())
                    })?;
                self.store.replace(target, old, new.to_string()).await
            }
            "remove" => {
                let old = args
                    .get("old_text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidArgs("old_text required for 'remove'".into())
                    })?;
                self.store.remove(target, old).await
            }
            "read" => self.store.read(target).await,
            other => {
                return Err(ToolError::InvalidArgs(format!(
                    "unknown action '{other}'; use add, replace, remove, read"
                )));
            }
        };

        let json = match result {
            Ok(value) => serde_json::json!({
                "success": true,
                "target": target,
                "entries": value.entries(),
                "entry_count": value.entry_count(),
            }),
            Err(err) => serde_json::json!({
                "success": false,
                "error": err.to_string(),
            }),
        };
        Ok(ToolOutput {
            content: serde_json::to_string(&json).map_err(|e| {
                ToolError::Execution(format!("failed to serialize memory result: {e}"))
            })?,
        })
    }
}

fn parse_target(value: Option<&Value>) -> Result<MemoryTarget, ToolError> {
    let s = value
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'target'".into()))?;
    match s {
        "memory" => Ok(MemoryTarget::Memory),
        "user" => Ok(MemoryTarget::User),
        other => Err(ToolError::InvalidArgs(format!(
            "invalid target '{other}'; use 'memory' or 'user'"
        ))),
    }
}

// Helper trait so we can serialize the result of both `MemoryOpResult`
// and `MemoryReadResult` uniformly.
trait MemoryResultLike {
    fn entries(&self) -> &[String];
    fn entry_count(&self) -> usize;
}
impl MemoryResultLike for super::store::MemoryOpResult {
    fn entries(&self) -> &[String] {
        &self.entries
    }
    fn entry_count(&self) -> usize {
        self.entry_count
    }
}
impl MemoryResultLike for super::store::MemoryReadResult {
    fn entries(&self) -> &[String] {
        &self.entries
    }
    fn entry_count(&self) -> usize {
        self.entry_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_store() -> (tempfile::TempDir, Arc<MemoryStore>) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = MemoryStore::load(super::super::store::MemoryConfig {
            memories_dir: dir.path().to_path_buf(),
        });
        let store = futures::executor::block_on(async { cfg.await.unwrap() });
        (dir, Arc::new(store))
    }

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".into(),
            working_dir: std::path::PathBuf::from("/tmp"),
            permissions: Default::default(),
        }
    }

    #[tokio::test]
    async fn add_action_returns_success_json() {
        let (_dir, store) = temp_store();
        let tool = MemoryTool::new(store);
        let out = tool
            .execute(
                json!({ "action": "add", "target": "memory", "content": "hello" }),
                ctx(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["success"], true);
        assert_eq!(v["target"], "memory");
        assert_eq!(v["entry_count"], 1);
    }

    #[tokio::test]
    async fn add_with_empty_content_returns_error_json() {
        let (_dir, store) = temp_store();
        let tool = MemoryTool::new(store);
        let out = tool
            .execute(
                json!({ "action": "add", "target": "memory", "content": "  " }),
                ctx(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["success"], false);
        assert!(v["error"].as_str().unwrap().contains("content"));
    }

    #[tokio::test]
    async fn missing_action_returns_invalid_args() {
        let (_dir, store) = temp_store();
        let tool = MemoryTool::new(store);
        let err = tool
            .execute(
                json!({ "target": "memory" }),
                ctx(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn missing_target_returns_invalid_args() {
        let (_dir, store) = temp_store();
        let tool = MemoryTool::new(store);
        let err = tool
            .execute(
                json!({ "action": "read" }),
                ctx(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn invalid_target_returns_invalid_args() {
        let (_dir, store) = temp_store();
        let tool = MemoryTool::new(store);
        let err = tool
            .execute(
                json!({ "action": "read", "target": "global" }),
                ctx(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn unknown_action_returns_invalid_args() {
        let (_dir, store) = temp_store();
        let tool = MemoryTool::new(store);
        let err = tool
            .execute(
                json!({ "action": "purge", "target": "memory" }),
                ctx(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn read_action_returns_empty_entries() {
        let (_dir, store) = temp_store();
        let tool = MemoryTool::new(store);
        let out = tool
            .execute(
                json!({ "action": "read", "target": "memory" }),
                ctx(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["success"], true);
        assert_eq!(v["entry_count"], 0);
        assert_eq!(v["entries"].as_array().unwrap().len(), 0);
    }
}
```

- [ ] **Step 2: Re-enable the `tool` module in `mod.rs`**

Edit `crates/hermes-skill-tools/src/tools/memory/mod.rs`. Replace the temporary commented-out lines with:

```rust
pub mod store;
pub mod tool;
```

(Already correct in the original `mod.rs` we wrote in Task 3 step 2; we just need to confirm — the file we wrote does NOT have the comments, so no edit needed. Verify by re-reading the file.)

- [ ] **Step 3: Run the tool tests**

Run: `cargo test -p perry-hermes-skill-tools --lib tools::memory::tool::`
Expected: 7 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/hermes-skill-tools/src/tools/memory/tool.rs
git commit -m "feat(skill-tools): add MemoryTool

LLM-facing tool for add/replace/remove/read of MEMORY.md and
USER.md entries. Disambiguates missing fields and unknown actions
into ToolError::InvalidArgs."
```

---

## Task 5: Add `MemoryBlock` and `resolve_memories_dir` to `hermes-agent::prompting`

**Files:**
- Modify: `crates/hermes-agent/src/prompting.rs`

- [ ] **Step 1: Add `resolve_memories_dir` next to `resolve_skills_dir`**

Find the existing `resolve_skills_dir` function. Right after its closing `}`, add:

```rust
/// Resolve the local memories directory, mirroring the rules used by
/// [`resolve_skills_dir`].
///
/// 1. `PERRY_HERMES_HOME` env var if set
/// 2. else `$HOME/.perry_hermes`
/// 3. else `./.perry_hermes`
/// 4. append `/memories`
pub fn resolve_memories_dir() -> Option<PathBuf> {
    let base = std::env::var_os("PERRY_HERMES_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".perry_hermes")))
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|cwd| cwd.join(".perry_hermes"))
        })?;
    Some(base.join("memories"))
}
```

- [ ] **Step 2: Add `MemoryBlock` after `AgentsMdBlock`**

Find the `impl PromptContextBlock for AgentsMdBlock` block. Add the following after its closing `}`:

```rust
/// Global block that reads the live entries from a [`MemoryStore`]
/// and renders them as a system-prompt section. One block per
/// [`MemoryTarget`].
///
/// The block reads from the in-memory `LiveState` rather than the
/// disk file. The agent calls `build_system_message` once per session
/// and freezes the result in `AgentSession.system_message`, so the
/// rendered block is effectively immutable for the session's lifetime
/// even though the store itself is mutable.
pub struct MemoryBlock {
    store: Arc<perry_hermes_skill_tools::tools::memory::MemoryStore>,
    target: perry_hermes_skill_tools::tools::memory::MemoryTarget,
    name_label: &'static str,
}

impl MemoryBlock {
    pub fn memory(
        store: Arc<perry_hermes_skill_tools::tools::memory::MemoryStore>,
    ) -> Self {
        Self {
            store,
            target: perry_hermes_skill_tools::tools::memory::MemoryTarget::Memory,
            name_label: "MEMORY",
        }
    }

    pub fn user(
        store: Arc<perry_hermes_skill_tools::tools::memory::MemoryStore>,
    ) -> Self {
        Self {
            store,
            target: perry_hermes_skill_tools::tools::memory::MemoryTarget::User,
            name_label: "USER",
        }
    }
}

#[async_trait]
impl PromptContextBlock for MemoryBlock {
    fn name(&self) -> &str {
        self.name_label
    }

    async fn load(&self) -> Option<String> {
        let entries = self.store.entries(self.target).await;
        if entries.is_empty() {
            return None;
        }
        Some(entries.join("\n\n"))
    }
}
```

- [ ] **Step 3: Add tests for `MemoryBlock` and `resolve_memories_dir`**

In `crates/hermes-agent/src/prompting.rs`, inside the existing `#[cfg(test)] mod tests` block, append:

```rust
    #[test]
    fn resolve_memories_dir_returns_path_ending_in_memories() {
        let _guard = crate::test_env::blocking_lock();
        let home = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("PERRY_HERMES_HOME", home.path()) };
        let dir = resolve_memories_dir().expect("memories dir should resolve");
        assert_eq!(
            dir.file_name().and_then(|s| s.to_str()),
            Some("memories")
        );
        unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
    }

    #[tokio::test]
    async fn memory_block_loads_entries_joined_by_blank_line() {
        use perry_hermes_skill_tools::tools::memory::{MemoryConfig, MemoryStore};
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(
            MemoryStore::load(MemoryConfig {
                memories_dir: tmp.path().to_path_buf(),
            })
            .await
            .unwrap(),
        );
        store.add(MemoryTarget::Memory, "first".into()).await.unwrap();
        store.add(MemoryTarget::Memory, "second".into()).await.unwrap();

        let block = MemoryBlock::memory(store);
        let body = block.load().await.expect("non-empty store should load");
        assert_eq!(body, "first\n\nsecond");
        assert_eq!(block.name(), "MEMORY");
    }

    #[tokio::test]
    async fn memory_block_returns_none_for_empty_store() {
        use perry_hermes_skill_tools::tools::memory::{MemoryConfig, MemoryStore};
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(
            MemoryStore::load(MemoryConfig {
                memories_dir: tmp.path().to_path_buf(),
            })
            .await
            .unwrap(),
        );
        let block = MemoryBlock::user(store);
        assert!(block.load().await.is_none());
        assert_eq!(block.name(), "USER");
    }
```

(You will also need to import `MemoryTarget` at the top of the test module; add this use line near the top of `mod tests`:)

```rust
use perry_hermes_skill_tools::tools::memory::MemoryTarget;
```

- [ ] **Step 4: Build `hermes-agent`**

Run: `cargo build -p perry-hermes-agent`
Expected: errors in `loop_engine/agent_loop.rs` (caller of `build_system_message` still sync). Other parts compile.

- [ ] **Step 5: Run the new tests**

Run: `cargo test -p perry-hermes-agent --lib prompting::`
Expected: all tests pass (9 from Task 2 + 3 new = 12).

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-agent/src/prompting.rs
git commit -m "feat(agent): add MemoryBlock and resolve_memories_dir

MemoryBlock implements PromptContextBlock to render memory entries
into the system prompt. resolve_memories_dir mirrors the skills-dir
resolver."
```

---

## Task 6: Wire `blocks` into `LoopConfig` and `AgentLoop::new_session`

**Files:**
- Modify: `crates/hermes-agent/src/loop_engine/agent_loop.rs`
- Modify: `crates/hermes-agent/src/loop_engine/run.rs` (if it calls `build_system_message`)
- Modify: `crates/hermes-agent/src/tool_catalog.rs`

- [ ] **Step 1: Verify `run.rs` usage**

Run: `grep -n "build_system_message" crates/hermes-agent/src/loop_engine/run.rs`
Expected: no matches. (Confirming `run.rs` does not call `build_system_message` directly — it only uses `system_message_for`.)

- [ ] **Step 2: Add `blocks` field to `LoopConfig`**

In `crates/hermes-agent/src/loop_engine/agent_loop.rs`, find the `LoopConfig` struct (around line 38) and add the new field:

```rust
#[derive(Clone)]
pub struct LoopConfig {
    pub max_iterations: u32,
    pub max_duration: Duration,
    pub system_prompt: Option<String>,
    /// Optional context compaction strategy. None = no compaction.
    pub compaction_strategy: Option<Arc<TokioMutex<dyn CompactionStrategy>>>,
    /// Model context window and compression threshold used with real
    /// provider usage.
    pub context_window: Option<ContextWindow>,
    /// Focus topic for manual `/compact [focus]`.
    pub focus_topic: Option<String>,
    /// Context blocks to inject into the system prompt at session
    /// creation. Each block is loaded once via `block.load().await`
    /// and the rendered result is frozen in `AgentSession.system_message`.
    pub blocks: Vec<Arc<dyn PromptContextBlock>>,
}
```

Also update the `Debug` impl (around line 67) to include the new field (use a placeholder label, as for `compaction_strategy`):

```rust
impl std::fmt::Debug for LoopConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopConfig")
            .field("max_iterations", &self.max_iterations)
            .field("max_duration", &self.max_duration)
            .field("system_prompt", &self.system_prompt)
            .field("compaction_strategy", &"<dyn CompactionStrategy>")
            .field("context_window", &self.context_window)
            .field("focus_topic", &self.focus_topic)
            .field("blocks", &format!("<[{} dyn PromptContextBlock]>", self.blocks.len()))
            .finish()
    }
}
```

Update `Default for LoopConfig` (around line 80) to add `blocks: Vec::new()`:

```rust
impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: 90,
            max_duration: Duration::from_secs(60 * 10),
            system_prompt: None,
            compaction_strategy: None,
            context_window: None,
            focus_topic: None,
            blocks: Vec::new(),
        }
    }
}
```

- [ ] **Step 3: Add the import for `PromptContextBlock`**

In `crates/hermes-agent/src/loop_engine/agent_loop.rs`, find the import:

```rust
use perry_hermes_core::tool::{ToolContext, ToolOutput};
```

Add a new import line right after it:

```rust
use perry_hermes_core::prompt_context::PromptContextBlock;
```

- [ ] **Step 4: Make `system_message_for` async**

Find `pub fn system_message_for(&self, working_dir: &std::path::Path) -> Option<Message> { build_system_message(working_dir) }` and replace with:

```rust
/// Build the system message for a session at `working_dir`.
/// Includes the hardcoded base prompt, skills index, all configured
/// [`PromptContextBlock`]s, and working directory hint. The blocks
/// list is taken from the `LoopConfig` and is iterated once at
/// session creation.
pub async fn system_message_for(
    &self,
    working_dir: &std::path::Path,
) -> Option<Message> {
    build_system_message(working_dir, &self.config.blocks).await
}
```

- [ ] **Step 5: Update `new_session` and `load_json_session` to await**

Find `new_session` (around line 222) and update:

```rust
    pub fn new_session(
        &self,
        session_id: impl Into<String>,
        working_dir: impl Into<PathBuf>,
    ) -> AgentSession {
        let working_dir = working_dir.into();
        let system_message = self.system_message_for(&working_dir);
        AgentSession::new(session_id, working_dir, system_message)
    }
```

Replace with:

```rust
    pub async fn new_session(
        &self,
        session_id: impl Into<String>,
        working_dir: impl Into<PathBuf>,
    ) -> AgentSession {
        let working_dir = working_dir.into();
        let system_message = self.system_message_for(&working_dir).await;
        AgentSession::new(session_id, working_dir, system_message)
    }
```

Find `load_json_session` (around line 241) and update:

```rust
    pub async fn load_json_session(
        &self,
        path: impl Into<PathBuf>,
    ) -> std::io::Result<AgentSession> {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let system_message = self.system_message_for(&working_dir);
        AgentSession::load_json_file_with_system_message(path, Some(working_dir), system_message)
            .await
    }
```

Replace with:

```rust
    pub async fn load_json_session(
        &self,
        path: impl Into<PathBuf>,
    ) -> std::io::Result<AgentSession> {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let system_message = self.system_message_for(&working_dir).await;
        AgentSession::load_json_file_with_system_message(path, Some(working_dir), system_message)
            .await
    }
```

- [ ] **Step 6: Update `build_loop_for_custom_provider` to wire blocks**

Find the `build_loop_for_custom_provider` function (around line 427) and update it to load the memory store, build the blocks list, and pass it into `LoopConfig`. Replace the function body with:

```rust
fn build_loop_for_custom_provider(
    provider: Arc<dyn Provider>,
    config: &PerryHermesConfig,
    selected_provider: Option<&ResolvedProviderConfig>,
) -> AgentLoop {
    let skills_dir = resolve_skills_dir().unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".perry_hermes")
            .join("skills")
    });
    let memories_dir = crate::prompting::resolve_memories_dir().unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".perry_hermes")
            .join("memories")
    });

    // Load the memory store synchronously here. The store is small and
    // bound to disk I/O for two files; spawn a one-shot blocking read.
    let memory_store = {
        let cfg = perry_hermes_skill_tools::tools::memory::MemoryConfig {
            memories_dir: memories_dir.clone(),
        };
        match futures::executor::block_on(perry_hermes_skill_tools::tools::memory::MemoryStore::load(cfg)) {
            Ok(store) => Some(Arc::new(store)),
            Err(err) => {
                tracing::warn!("failed to load memory store: {err}; continuing without memory blocks");
                None
            }
        }
    };

    let registry = Arc::new(build_registry(
        &config.agent.disabled_toolsets,
        &skills_dir,
        memory_store.clone(),
    ));
    let compaction_strategy = if config.agent.context_compression_enabled {
        let compactor_config = CompactorConfig::default();
        Some(Arc::new(TokioMutex::new(
            SummaryCompactor::new(compactor_config).with_summary_provider(Arc::clone(&provider)),
        )) as Arc<TokioMutex<dyn CompactionStrategy>>)
    } else {
        None
    };
    let context_window = selected_provider.map(|provider| ContextWindow {
        max_tokens: provider.context_window_size,
        compression_threshold_ratio: config
            .agent
            .context_compression_threshold_percent
            .unwrap_or(0.50),
    });

    let mut blocks: Vec<Arc<dyn PromptContextBlock>> = vec![
        Arc::new(crate::prompting::AgentsMdBlock::new(
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        )),
    ];
    if let Some(store) = &memory_store {
        blocks.push(Arc::new(crate::prompting::MemoryBlock::memory(store.clone())));
        blocks.push(Arc::new(crate::prompting::MemoryBlock::user(store.clone())));
    }

    AgentLoop::from_parts(
        provider,
        registry,
        LoopConfig {
            max_iterations: config.agent.max_iterations.unwrap_or(10),
            system_prompt: None,
            compaction_strategy,
            context_window,
            blocks,
            ..Default::default()
        },
    )
}
```

- [ ] **Step 7: Add `perry-hermes-skill-tools` import**

In `crates/hermes-agent/src/loop_engine/agent_loop.rs`, find the import for `perry_hermes_skill_tools` (if it exists) or add a new one. Look for any `use perry_hermes_skill_tools::` line and add right after it (or add a new one if absent):

```rust
use perry_hermes_skill_tools::tools::memory;
```

(If `memory` is too generic and conflicts with other usages, use the full path inline; the existing code in step 6 already uses `perry_hermes_skill_tools::tools::memory::MemoryConfig` directly, so an extra import is not strictly required.)

- [ ] **Step 8: Update `tool_catalog.rs` to accept `Arc<MemoryStore>`**

Edit `crates/hermes-agent/src/tool_catalog.rs`. Find:

```rust
pub fn build_registry(disabled_toolsets: &[String], skills_dir: &Path) -> InMemoryRegistry {
```

Replace with:

```rust
pub fn build_registry(
    disabled_toolsets: &[String],
    skills_dir: &Path,
    memory_store: Option<Arc<perry_hermes_skill_tools::tools::memory::MemoryStore>>,
) -> InMemoryRegistry {
```

Add the `Arc` import at the top of the file. The existing file has `use std::path::Path;`. Add `use std::sync::Arc;` to the imports.

Then add the `MemoryTool` registration at the end of the function body, right before the final `reg`:

```rust
    if !disabled_toolsets.iter().any(|s| s == "memory") {
        if let Some(store) = memory_store {
            reg = reg.register(Arc::new(
                perry_hermes_skill_tools::tools::memory::MemoryTool::new(store),
            ));
        }
    }
    reg
}
```

(We register the tool only when a store is available; if `load` failed and the caller passed `None`, we skip silently. Tests that pass `None` get no `memory` tool.)

- [ ] **Step 9: Update existing `tool_catalog` tests**

In `crates/hermes-agent/src/tool_catalog.rs`, the test module has 5 tests that call `build_registry`. Update each to pass `None` as the third argument:

- `runtime_disables_terminal_toolset_from_registry`
- `legacy_core_disables_shell_tool`
- `file_toolset_disables_read_write_patch_and_search`
- `skills_toolset_disables_list_and_view`
- `default_registry_includes_all_seven_tools`
- `patch_schema_carries_reference_parameters`
- `search_files_schema_carries_reference_parameters`

For each, change the call from:

```rust
let registry = build_registry(&["terminal".to_string()], &test_skills_dir());
```

to:

```rust
let registry = build_registry(&["terminal".to_string()], &test_skills_dir(), None);
```

Also add a new test verifying `MemoryTool` registers when a store is provided:

```rust
    #[tokio::test]
    async fn memory_tool_registers_when_store_provided() {
        use perry_hermes_skill_tools::tools::memory::{MemoryConfig, MemoryStore};
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(
            MemoryStore::load(MemoryConfig {
                memories_dir: tmp.path().to_path_buf(),
            })
            .await
            .unwrap(),
        );
        let registry = build_registry(&[], &test_skills_dir(), Some(store));
        let names: Vec<_> = registry.schemas().into_iter().map(|s| s.name).collect();
        assert!(names.iter().any(|n| n == "memory"));
    }

    #[test]
    fn memory_tool_absent_when_no_store_provided() {
        let registry = build_registry(&[], &test_skills_dir(), None);
        let names: Vec<_> = registry.schemas().into_iter().map(|s| s.name).collect();
        assert!(!names.iter().any(|n| n == "memory"));
    }
```

- [ ] **Step 10: Update existing agent_loop tests**

In `crates/hermes-agent/src/loop_engine/agent_loop.rs`, find every test that calls `agent.new_session(...)` or `agent.load_json_session(...)` synchronously and add `.await`. There are at least 2 such tests (the ones that load sessions). Also update any test that calls `agent.system_message_for(...)` to add `.await`.

Run `cargo build -p perry-hermes-agent --tests` to find the call sites. The compiler will report each one. Apply the fix in each location:

- For `agent.new_session("k", path)`, change to `agent.new_session("k", path).await`.
- For `agent.load_json_session(path)`, the method is already `async`, so no change needed at the call site (just ensure callers `.await` if they aren't already).
- For any `agent.system_message_for(path)`, add `.await`.

For tests using `LoopConfig::default()`, no change — `blocks: Vec::new()` is the default.

For tests that construct `LoopConfig { ... }` explicitly, add `blocks: Vec::new(),` (or `..Default::default()`) as appropriate.

- [ ] **Step 11: Build entire workspace**

Run: `cargo build --workspace`
Expected: success. (Tests may still fail at runtime; this just checks compilation.)

- [ ] **Step 12: Run agent tests**

Run: `cargo test -p perry-hermes-agent --lib`
Expected: all tests pass.

- [ ] **Step 13: Commit**

```bash
git add crates/hermes-agent/src/loop_engine/agent_loop.rs crates/hermes-agent/src/tool_catalog.rs
git commit -m "feat(agent): thread PromptContextBlocks through AgentLoop

LoopConfig gains a blocks field. AgentLoop::new_session and
load_json_session now await an async system_message_for. The
build_loop_for_custom_provider helper loads MemoryStore and
constructs the standard [AgentsMd, Memory, User] block list.
MemoryTool is registered by default unless disabled_toolsets
contains \"memory\"."
```

---

## Task 7: Update CLI/TUI integration tests and verify the full build

**Files:**
- Modify: integration tests in `crates/hermes-cli/tests/` and any others that call `AgentLoop::new_session` / `system_message_for`

- [ ] **Step 1: Find all call sites that need `.await`**

Run: `cargo build --workspace --tests 2>&1 | head -60`
Expected: a list of compile errors. Apply `.await` to each call of `new_session`, `system_message_for` (and any other now-async methods) that we missed.

- [ ] **Step 2: Find all `LoopConfig` initializations that need a `blocks` field**

Run: `cargo build --workspace --tests 2>&1 | grep "missing field" | head -20`
Expected: any test that constructs `LoopConfig { ... }` without `..Default::default()` and without `blocks` will be reported. Add `blocks: Vec::new(),` to each.

- [ ] **Step 3: Build everything**

Run: `cargo build --workspace --tests`
Expected: success.

- [ ] **Step 4: Run the entire workspace test suite**

Run: `cargo test --workspace`
Expected: all tests pass.

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings. If there are warnings, fix them in the same commit. Common issues to watch for:
- `needless_collect` on `.await` results
- `unused_imports` for MemoryTarget after refactor
- `clippy::field_reassign_with_default` in tests (already `#[allow]`'d in the existing module)

- [ ] **Step 6: Smoke test CLI end-to-end (manual)**

Run: `cargo run --bin hermes -- tui 2>&1 | head -20` (this is a TUI; Ctrl+C to exit)
Expected: TUI launches without panic.

- [ ] **Step 7: Final commit if any fixups were needed**

```bash
git add -A
git commit -m "chore: fix clippy warnings and downstream test call sites"
```

(Only if step 5 produced changes.)

---

## Self-Review

**1. Spec coverage:**
- Goal (generalize AGENTS.md injection): covered in Task 2.
- `PromptContextBlock` trait in `hermes-core`: Task 1.
- `AgentsMdBlock` in `hermes-agent::prompting`: Task 2.
- `build_system_message` async, takes `&[Arc<dyn PromptContextBlock>]`: Task 2.
- Existing AGENTS.md behavior preserved (byte-equivalent for empty blocks list): Task 2 tests + conversion of existing tests.
- `MemoryStore` with MEMORY.md + USER.md: Task 3.
- `MemoryError` shape `{kind, message}`: Task 3.
- `MemoryTool` with 4 actions: Task 4.
- `MemoryBlock` in `hermes-agent::prompting`: Task 5.
- `resolve_memories_dir`: Task 5.
- `LoopConfig` carries `blocks`: Task 6.
- `AgentLoop` threads blocks through `new_session` / `load_json_session`: Task 6.
- `tool_catalog` registers `MemoryTool` (default on, disable via `disabled_toolsets`): Task 6.
- `fs2` flock + tempfile+rename concurrency: Task 3.
- All test categories in spec: covered in Task 2/3/4/5/6.
- File-by-file change summary: matches the table.

**2. Placeholder scan:** No "TBD" / "TODO" / "implement later" in the plan. All code blocks are complete. All commands have expected output. All references to types/functions resolve within the plan.

**3. Type consistency:**
- `MemoryTarget` is in `perry_hermes_skill_tools::tools::memory` (Task 3), referenced consistently in Tasks 4, 5, 6.
- `MemoryError` is in `perry_hermes_skill_tools::tools::memory::store`, referenced in Task 4.
- `MemoryOpResult` / `MemoryReadResult` defined in Task 3, used in Task 4 via the `MemoryResultLike` helper trait.
- `MemoryConfig` defined in Task 3, used in Tasks 4, 5, 6.
- `MemoryStore::entries` is async, used in `MemoryBlock::load` (Task 5) which is also async.
- `LoopConfig::blocks` field added in Task 6, used in `system_message_for`, `new_session`, `load_json_session`.
- `build_system_message` is async, called with `.await` in `system_message_for`.
- `tool_catalog::build_registry` takes 3 args (added in Task 6), called consistently from `build_loop_for_custom_provider` and updated in all tests.

One thing to double-check: Task 6 step 6 uses `futures::executor::block_on` for the memory store load. This is sync-blocking, which is OK at startup but could be replaced with a one-shot `tokio::task::spawn_blocking` if it ever shows up in profiles. The plan notes this.

Another: Task 6 step 6 uses `std::env::current_dir()` for the `AgentsMdBlock` working dir, but the spec said the block should be relative to the session's working dir (not process cwd). This is intentional: the `AgentLoop` doesn't know the session's working dir at construction time (sessions pick their own per-message). The blocks list is shared across all sessions in this agent, so the `current_dir()` is a reasonable default — the per-session override happens via `system_message_for(working_dir)`, but the blocks themselves don't currently receive the per-session `working_dir`. This is a known limitation. If a future block type needs per-session cwd, the `PromptContextBlock` trait can grow a `load(working_dir)` method. For now, `AgentsMdBlock` uses its own captured `working_dir` (set at construction). Adjusting to per-session cwd would require passing `working_dir` into `load()`, which is a separate refactor.

The plan as written captures the design as approved in the spec, with one minor adjustment: the `AgentsMdBlock` working dir is captured at agent construction time (from `std::env::current_dir()`), matching today's behavior where the same cwd is used for all sessions. This is the right level of fidelity for the refactor.
