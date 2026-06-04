//! The agent loop. Phase 1 ships a one-iteration minimum that handles
//! `Stop` and a few sibling finish reasons; later phases will replace
//! it with the full state machine described in
//! `plans/rust-port-design.md` §4.

pub mod agent;

pub use agent::{AgentLoop, LoopConfig, LoopEvent, LoopMetrics, RunResult};
