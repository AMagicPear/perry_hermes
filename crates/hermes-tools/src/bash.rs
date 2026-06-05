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
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

/// Truncate output keeping a head+tail strategy (aligned with Python Hermes).
/// Preserves the first 40% and last 60% of the character budget, since error
/// messages often appear early and recent output is usually more relevant.
fn truncate_output(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        return s.to_string();
    }
    let head_chars = max_chars * 2 / 5; // 40%
    let tail_chars = max_chars - head_chars; // 60%
    let head: String = s.chars().take(head_chars).collect();
    let tail: String = s.chars().skip(char_count - tail_chars).collect();
    let omitted = char_count - head_chars - tail_chars;
    format!(
        "{head}\n\n... [OUTPUT TRUNCATED - {omitted} chars omitted out of {char_count} total] ...\n\n{tail}"
    )
}

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
        if !ctx.permissions.subprocess {
            return Err(ToolError::Permission(
                "subprocess execution not permitted".into(),
            ));
        }
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
            // Concurrently drain stdout and stderr to avoid pipe deadlock:
            // if the child fills one pipe buffer while we're blocked reading
            // the other, both sides stall and we hit the timeout.
            result = async {
                let (stdout_bytes, stderr_bytes) = tokio::join!(
                    async {
                        let mut buf = Vec::new();
                        if let Some(mut s) = child.stdout.take() {
                            let _ = s.read_to_end(&mut buf).await;
                        }
                        buf
                    },
                    async {
                        let mut buf = Vec::new();
                        if let Some(mut s) = child.stderr.take() {
                            let _ = s.read_to_end(&mut buf).await;
                        }
                        buf
                    }
                );
                let status = child.wait().await
                    .map_err(|e| ToolError::Execution(e.to_string()))?;
                Ok::<_, ToolError>((stdout_bytes, stderr_bytes, status))
            } => {
                let (stdout_bytes, stderr_bytes, status) = result?;
                let out = String::from_utf8_lossy(&stdout_bytes).into_owned();
                let err = String::from_utf8_lossy(&stderr_bytes).into_owned();
                let combined = if err.is_empty() {
                    out
                } else if out.is_empty() {
                    err
                } else {
                    format!("{out}\n--- stderr ---\n{err}")
                };
                let truncated = truncate_output(&combined, 50_000);
                let exit_note = if status.success() {
                    String::new()
                } else {
                    format!("\n[exit code {}]", status.code().unwrap_or(-1))
                };
                Ok(ToolOutput {
                    content: format!("{truncated}{exit_note}"),
                })
            }
        }
    }
}
