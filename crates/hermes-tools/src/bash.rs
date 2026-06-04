//! `BashTool` — run a shell command, return its combined output.
//!
//! Phase 3 minimum: spawn `bash -c <command>`, capture stdout + stderr,
//! return as `ToolOutput.content` with a non-zero-exit footer. No
//! sandboxing yet — don't run this on a machine you care about. A
//! later phase will move this into a WASM/sandbox per the design doc.

use std::process::Stdio;

use async_trait::async_trait;
use hermes_core::error::ToolError;
use hermes_core::tool::{Tool, ToolContext, ToolOutput};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tokio::time::Duration;

pub struct BashTool;

impl BashTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command and return its combined stdout+stderr. \
         Use for file operations, running scripts, inspecting the system, etc."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Maximum wall-clock seconds before the command is killed.",
                    "default": 30,
                    "minimum": 1,
                    "maximum": 600
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn toolset(&self) -> &'static str {
        "core"
    }

    async fn execute(
        &self,
        args: Value,
        ctx: ToolContext,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("missing 'command'".into()))?;
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(30);

        let mut child = Command::new("bash")
            .arg("-c")
            .arg(command)
            .current_dir(&ctx.working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ToolError::Execution(e.to_string()))?;

        let timeout = Duration::from_secs(timeout_secs);
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                return Err(ToolError::Cancelled);
            }
            _ = tokio::time::sleep(timeout) => {
                let _ = child.kill().await;
                return Err(ToolError::Timeout(timeout_secs));
            }
            status = child.wait() => {
                let status = status.map_err(|e| ToolError::Execution(e.to_string()))?;
                let mut out = String::new();
                if let Some(mut s) = child.stdout.take() {
                    let _ = s.read_to_string(&mut out).await;
                }
                let mut err = String::new();
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut err).await;
                }
                let combined = if err.is_empty() {
                    out
                } else if out.is_empty() {
                    err
                } else {
                    format!("{out}\n--- stderr ---\n{err}")
                };
                // Truncate very large outputs to keep the context window sane.
                let truncated = if combined.len() > 50_000 {
                    format!(
                        "{}\n... [truncated, full output {} bytes] ...",
                        &combined[..25_000],
                        combined.len()
                    )
                } else {
                    combined
                };
                let exit_note = if status.success() {
                    String::new()
                } else {
                    format!("\n[exit code {}]", status.code().unwrap_or(-1))
                };
                Ok(ToolOutput {
                    content: format!("{truncated}{exit_note}"),
                    attachments: vec![],
                })
            }
        }
    }
}
