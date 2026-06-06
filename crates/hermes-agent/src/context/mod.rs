//! Context compression subsystem.
//!
//! When conversation history approaches the model's context limit, the
//! compressor trims old tool outputs and generates an LLM summary of the
//! middle turns while preserving the head and tail of the conversation.

mod compressor;
mod pruning;
mod summary;

pub use compressor::{CompressorConfig, ContextCompressor};
