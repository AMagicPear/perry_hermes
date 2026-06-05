//! Core types and traits for the Hermes agent loop.
//!
//! This crate has no IO, no async (beyond trait method signatures), and no
//! dependencies beyond serde, tokio-util, async-trait, and thiserror. It is
//! intended to compile in ~1s and to be trivially mockable from every other
//! crate in the workspace.

pub mod accumulator;
pub mod error;
pub mod message;
pub mod provider;
pub mod registry;
pub mod tool;
pub mod usage;

pub use error::{LoopError, ProviderError, ToolError};
pub use message::{Content, ContentPart, Message, Role, ToolCall};
pub use provider::{Completion, CompletionDelta, CompletionStream, FinishReason, Provider};
pub use registry::{InMemoryRegistry, ToolSchema};
pub use tool::{Tool, ToolContext, ToolOutput, ToolPermissions};
pub use usage::Usage;
