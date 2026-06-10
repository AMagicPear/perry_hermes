//! `PromptContextBlock` — a context fragment loaded at session
//! creation and frozen into the system prompt.

use async_trait::async_trait;
use std::path::Path;

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

    /// Load the block scoped to a specific working directory.
    /// Override this in blocks that need per-session working directory
    /// resolution (e.g. `AgentsMdBlock`). Default implementation calls
    /// `load()`.
    async fn load_for(&self, _working_dir: &Path) -> Option<String> {
        self.load().await
    }
}
