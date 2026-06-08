//! The `Tool` trait — something the LLM can ask to be invoked.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::error::ToolError;

/// A callable unit of capability exposed to the LLM. Built-in implementations
/// live in `perry-hermes-agent` and are registered into an
/// [`InMemoryRegistry`](crate::registry::InMemoryRegistry) at startup.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;

    /// JSON Schema (draft-07) describing the tool's arguments. Returned to
    /// the LLM verbatim so it can emit valid tool calls.
    fn parameters_schema(&self) -> Value;

    /// Which toolset this tool belongs to. Toolsets are the unit of
    /// enable/disable (`"core"` tools are always on, `"mcp"` tools are
    /// platform-gated, etc.). Required — every tool must declare its
    /// toolset so the loop and CLI can apply `enabled_toolsets` filtering.
    fn toolset(&self) -> &'static str;

    /// Whether the tool is async-only. Mirrors the hermes-agent `is_async`
    /// field — used by hosts that want to schedule sync vs async work.
    fn is_async(&self) -> bool {
        false
    }

    /// Environment variables the tool needs to be useful. Mirrors the
    /// hermes-agent `requires_env` field — hosts can warn the user when
    /// any required env is missing.
    fn requires_env(&self) -> &[&str] {
        &[]
    }

    /// Optional cap on the size of the tool's result. Mirrors the
    /// hermes-agent `max_result_size_chars` field — when set, the host
    /// should truncate the `content` string to roughly this many chars
    /// before handing it to the model.
    fn max_result_size_chars(&self) -> Option<usize> {
        None
    }

    /// Optional emoji hint for UI surfaces that render the tool list.
    /// Mirrors the hermes-agent `emoji` field.
    fn emoji(&self) -> Option<&str> {
        None
    }

    /// Whether the tool is currently available. Mirrors the hermes-agent
    /// `check_fn` field — hosts can disable tools whose external
    /// dependencies are not present without removing them from the
    /// schema entirely.
    fn check_available(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        args: Value,
        ctx: ToolContext,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError>;
}

/// Per-invocation context passed to a tool. Carries the session id, the
/// working directory, and the resolved permission policy.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub session_id: String,
    pub working_dir: PathBuf,
    /// Per-tool permission policy. The registry resolves this; tools get the
    /// answer through here, not by checking config themselves.
    pub permissions: ToolPermissions,
}

/// Capability flags. The loop / registry decides which flags to set; tools
/// should consult this rather than reading global config.
#[derive(Debug, Clone, Default)]
pub struct ToolPermissions {
    pub subprocess: bool,
}

/// What a tool returns. The `content` string is fed back to the LLM as the
/// `role: tool` message.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: String,
}
