//! `ProcessTool` — the `process` tool, exposed to the model as `"process"`.
//!
//! Manages background processes started with `terminal(background=true).
//! Actions: list, poll, log, wait, kill.

use async_trait::async_trait;
use perry_hermes_core::error::ToolError;
use perry_hermes_core::tool::{Tool, ToolContext, ToolOutput};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::process_registry::PROCESS_REGISTRY;

/// The model-visible description for the `process` tool.
const PROCESS_TOOL_DESCRIPTION: &str = "Manage background processes started with terminal(background=true). \
Actions: 'list' (show all), 'poll' (check status + new output), \
'log' (full output with pagination), 'wait' (block until done or timeout), \
'kill' (terminate).";

pub struct ProcessTool;

impl ProcessTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ProcessTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ProcessTool {
    fn name(&self) -> &str {
        "process"
    }

    fn description(&self) -> &str {
        PROCESS_TOOL_DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "poll", "log", "wait", "kill"],
                    "description": "Action to perform on background processes"
                },
                "session_id": {
                    "type": "string",
                    "description": "Process session ID (from terminal background output). Required for all actions except 'list'."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Max seconds to block for 'wait' action. Returns partial output on timeout.",
                    "minimum": 1
                },
                "offset": {
                    "type": "integer",
                    "description": "Line offset for 'log' action (default: last 200 lines)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max lines to return for 'log' action",
                    "minimum": 1
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn toolset(&self) -> &'static str {
        "terminal"
    }

    async fn execute(
        &self,
        args: Value,
        _ctx: ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("missing 'action'".into()))?;

        match action {
            "list" => {
                let list = PROCESS_REGISTRY.list().await;
                Ok(ToolOutput {
                    content: serde_json::to_string_pretty(&json!({
                        "processes": list.iter().map(|p| json!({
                            "id": p.id,
                            "command": p.command,
                            "status": p.status,
                            "exit_code": p.exit_code,
                            "uptime_secs": p.uptime_secs,
                            "output_chars": p.output_chars,
                        })).collect::<Vec<_>>(),
                    }))
                    .unwrap_or_else(|_| "{}".to_string()),
                })
            }
            "poll" => {
                let session_id = require_session_id(&args)?;
                let result = PROCESS_REGISTRY
                    .poll(&session_id)
                    .await
                    .map_err(ToolError::Execution)?;
                Ok(ToolOutput {
                    content: serde_json::to_string_pretty(&json!({
                        "session_id": result.session_id,
                        "status": result.status,
                        "exit_code": result.exit_code,
                        "output": result.output,
                        "uptime_secs": result.uptime_secs,
                    }))
                    .unwrap_or_else(|_| "{}".to_string()),
                })
            }
            "log" => {
                let session_id = require_session_id(&args)?;
                let offset = args
                    .get("offset")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize);
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize);
                let result = PROCESS_REGISTRY
                    .read_log(&session_id, offset, limit)
                    .await
                    .map_err(ToolError::Execution)?;
                Ok(ToolOutput {
                    content: serde_json::to_string_pretty(&json!({
                        "session_id": result.session_id,
                        "total_lines": result.total_lines,
                        "lines": result.lines,
                    }))
                    .unwrap_or_else(|_| "{}".to_string()),
                })
            }
            "wait" => {
                let session_id = require_session_id(&args)?;
                let timeout = args.get("timeout").and_then(|v| v.as_u64());
                let result = PROCESS_REGISTRY
                    .wait(&session_id, timeout)
                    .await
                    .map_err(ToolError::Execution)?;
                Ok(ToolOutput {
                    content: serde_json::to_string_pretty(&json!({
                        "session_id": result.session_id,
                        "exit_code": result.exit_code,
                        "output": result.output,
                        "timed_out": result.timed_out,
                    }))
                    .unwrap_or_else(|_| "{}".to_string()),
                })
            }
            "kill" => {
                let session_id = require_session_id(&args)?;
                let result = PROCESS_REGISTRY
                    .kill(&session_id)
                    .await
                    .map_err(ToolError::Execution)?;
                Ok(ToolOutput {
                    content: serde_json::to_string_pretty(&json!({
                        "session_id": result.session_id,
                        "killed": result.killed,
                        "exit_code": result.exit_code,
                    }))
                    .unwrap_or_else(|_| "{}".to_string()),
                })
            }
            other => Err(ToolError::InvalidArgs(format!(
                "unknown process action: {other}. Use: list, poll, log, wait, kill"
            ))),
        }
    }
}

fn require_session_id(args: &Value) -> Result<String, ToolError> {
    args.get("session_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            ToolError::InvalidArgs(
                "session_id is required for this action (use 'list' to see available processes)"
                    .into(),
            )
        })
}
