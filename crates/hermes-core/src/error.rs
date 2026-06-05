//! Error types for the provider, tool, and loop layers.

use crate::message::Message;
use thiserror::Error;

/// Errors that can occur when calling an LLM provider.
#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("rate limited (retry after {retry_after_secs}s)")]
    RateLimited { retry_after_secs: u64 },
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

/// Errors that can occur when executing a tool.
#[derive(Debug, Clone, Error)]
pub enum ToolError {
    #[error("tool not found: {0}")]
    NotFound(String),
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    #[error("execution failed: {0}")]
    Execution(String),
    #[error("permission denied: {0}")]
    Permission(String),
    #[error("cancelled")]
    Cancelled,
    #[error("timeout after {0}s")]
    Timeout(u64),
}

/// Errors that can terminate the agent loop.
#[derive(Debug, Error)]
pub enum LoopError {
    #[error("max iterations ({0}) reached")]
    MaxIterations(u32),
    #[error("timeout after {0:?}")]
    Timeout(std::time::Duration),
    #[error("cancelled")]
    Cancelled,
    #[error("cancelled with partial response")]
    CancelledWith(Message),
    #[error("content filter triggered")]
    ContentFilter,
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),
}
