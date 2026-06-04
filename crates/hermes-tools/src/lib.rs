//! Built-in tool implementations. Phase 3 ships `BashTool`; later
//! phases add Read/Write/Edit/WebSearch/...
//!
//! See `plans/rust-port-design.md` for the full roadmap.

pub mod bash;

pub use bash::BashTool;
