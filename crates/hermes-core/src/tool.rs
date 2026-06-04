//! The `Tool` trait — something the LLM can ask to be invoked.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::error::ToolError;

/// A callable unit of capability exposed to the LLM. Implementations live in
/// `hermes-tools` (bash, file ops, …) and are registered into a
/// [`ToolRegistry`](crate::registry::ToolRegistry) at startup.
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
    pub network: bool,
    pub filesystem_write: bool,
    pub subprocess: bool,
}

/// What a tool returns. The `content` string is fed back to the LLM as the
/// `role: tool` message; `attachments` are future-facing (v0 ignores them).
#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// Text fed back to the LLM as the `role: tool` message.
    pub content: String,
    /// Optional attachments (images, files) — not yet in v0.
    pub attachments: Vec<Attachment>,
}

/// A binary or file payload attached to a tool output.
#[derive(Debug, Clone)]
pub struct Attachment {
    pub kind: AttachmentKind,
    pub data: Vec<u8>,
    pub mime: String,
}

#[derive(Debug, Clone, Copy)]
pub enum AttachmentKind {
    Image,
    File,
    Audio,
}