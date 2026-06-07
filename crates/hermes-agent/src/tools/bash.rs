//! `BashTool` — the public `terminal` tool, exposed to the model as `"terminal"`.
//!
//! The Rust struct keeps the name `BashTool` (smaller diff against the existing
//! public re-export) but its public tool contract — `name()`, `toolset()`,
//! `description()`, `parameters_schema()` — is aligned with Python's
//! `TERMINAL_SCHEMA` so prompts and provider tool calls stay portable.

use std::process::Stdio;

use async_trait::async_trait;
use perry_hermes_core::error::ToolError;
use perry_hermes_core::tool::{Tool, ToolContext, ToolOutput};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

/// The model-visible description for the `terminal` tool.
///
/// Copied verbatim from Python's `TERMINAL_TOOL_DESCRIPTION` so prompts and
/// provider tool calls stay aligned.
pub const TERMINAL_TOOL_DESCRIPTION: &str = "Execute shell commands on a Linux environment. Filesystem usually persists between calls.\n\
\n\
Do NOT use cat/head/tail to read files — use read_file instead.\n\
Do NOT use grep/rg/find to search — use search_files instead.\n\
Do NOT use ls to list directories — use search_files(target='files') instead.\n\
Do NOT use sed/awk to edit files — use patch instead.\n\
Do NOT use echo/cat heredoc to create files — use write_file instead.\n\
Reserve terminal for: builds, installs, git, processes, scripts, network, package managers, and anything that needs a shell.\n\
\n\
Foreground (default): Commands return INSTANTLY when done, even if the timeout is high. Set timeout=300 for long builds/scripts — you'll still get the result in seconds if it's fast. Prefer foreground for short commands.\n\
Background: Set background=true to get a session_id. Almost always pair with notify_on_complete=true — bg without notify runs SILENTLY and you have no way to learn it finished short of calling process(action='poll') yourself. Two legitimate uses:\n\
  (1) Long-lived processes that never exit (servers, watchers, daemons) — silent is correct, there's no exit to notify on.\n\
  (2) Long-running bounded tasks (tests, builds, deploys, CI pollers, batch jobs) — MUST set notify_on_complete=true. Without it you'll either forget to poll or sit blocked waiting for the user to surface the result.\n\
For servers/watchers, do NOT use shell-level background wrappers (nohup/disown/setsid/trailing '&') in foreground mode. Use background=true so Perry Hermes can track lifecycle and output.\n\
After starting a server, verify readiness with a health check or log signal, then run tests in a separate terminal() call. Avoid blind sleep loops.\n\
Use process(action=\"poll\") for progress checks, process(action=\"wait\") to block until done.\n\
Working directory: Use 'workdir' for per-command cwd.\n\
PTY mode: Set pty=true for interactive CLI tools (Codex, Claude Code, Python REPL).\n\
\n\
Do NOT use vim/nano/interactive tools without pty=true — they hang without a pseudo-terminal. Pipe git output to cat if it might page.";

/// Default command timeout in seconds when the model does not specify one.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

pub(crate) fn truncate_output(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        return s.to_string();
    }
    let head_chars = max_chars * 2 / 5;
    let tail_chars = max_chars - head_chars;
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
        "terminal"
    }

    fn description(&self) -> &str {
        TERMINAL_TOOL_DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        // Mirrors Python's TERMINAL_SCHEMA. The Rust backend only honors a
        // subset of these fields today (command, workdir, timeout) and ignores
        // the rest; the schema advertises the full surface so prompts and
        // tool calls line up with Python.
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to execute on the VM"
                },
                "background": {
                    "type": "boolean",
                    "default": false,
                    "description": "Run the command in the background; returns a session_id"
                },
                "timeout": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum wall-clock seconds before the command is killed (default 30)"
                },
                "workdir": {
                    "type": "string",
                    "description": "Working directory for this command (absolute path). Defaults to the session working directory."
                },
                "pty": {
                    "type": "boolean",
                    "default": false,
                    "description": "Allocate a pseudo-terminal (for interactive CLIs)"
                },
                "notify_on_complete": {
                    "type": "boolean",
                    "default": false,
                    "description": "When background=true, notify when the process exits"
                },
                "watch_patterns": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Glob patterns to watch for changes while the command runs"
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn toolset(&self) -> &'static str {
        "terminal"
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
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        let workdir = args
            .get("workdir")
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from);
        let cwd = workdir.as_ref().unwrap_or(&ctx.working_dir);

        if args.get("background").and_then(|v| v.as_bool()) == Some(true) {
            return Err(ToolError::InvalidArgs(
                "background=true is not supported in this Rust runtime yet".into(),
            ));
        }
        if args.get("pty").and_then(|v| v.as_bool()) == Some(true) {
            return Err(ToolError::InvalidArgs(
                "pty=true is not supported in this Rust runtime yet".into(),
            ));
        }
        if args.get("notify_on_complete").and_then(|v| v.as_bool()) == Some(true) {
            return Err(ToolError::InvalidArgs(
                "notify_on_complete is not supported in this Rust runtime yet".into(),
            ));
        }
        if args
            .get("watch_patterns")
            .and_then(|v| v.as_array())
            .is_some_and(|patterns| !patterns.is_empty())
        {
            return Err(ToolError::InvalidArgs(
                "watch_patterns is not supported in this Rust runtime yet".into(),
            ));
        }

        let mut child = Command::new("bash")
            .arg("-c")
            .arg(command)
            .current_dir(cwd)
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
                Err(ToolError::Cancelled)
            }
            _ = tokio::time::sleep(timeout) => {
                let _ = child.kill().await;
                Err(ToolError::Timeout(timeout_secs))
            }
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
