//! Memory tool: persistent file-backed memory (MEMORY.md + USER.md).
//!
//! See `store.rs` for the data layer and `tool.rs` for the LLM-facing
//! tool. `MemoryBlock` (which renders the system-prompt snapshot)
//! lives in `hermes-agent::prompting` next to the other prompt blocks.

pub mod store;
pub mod tool;

pub use store::{
    MemoryConfig, MemoryError, MemoryOpResult, MemoryReadResult, MemoryStore, MemoryTarget,
};
pub use tool::MemoryTool;
