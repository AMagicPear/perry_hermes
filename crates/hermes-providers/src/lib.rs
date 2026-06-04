//! LLM provider implementations. Phase 1 ships `EchoProvider`; real
//! providers (OpenAI, Anthropic, …) land in later phases.
//!
//! See `plans/rust-port-design.md` for the full roadmap.

pub mod echo;

pub use echo::EchoProvider;
