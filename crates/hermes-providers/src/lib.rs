//! LLM provider implementations. Phase 1 ships `EchoProvider`; phase 2
//! adds `OpenAiProvider` (real Chat Completions via `reqwest`).
//!
//! See `plans/rust-port-design.md` for the full roadmap.

pub mod anthropic;
pub mod echo;
pub mod openai;

pub use anthropic::{AnthropicProvider, AnthropicRequestOptions, AnthropicThinking};
pub use echo::EchoProvider;
pub use openai::OpenAiProvider;
