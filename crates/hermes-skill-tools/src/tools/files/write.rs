use async_trait::async_trait;
use perry_hermes_core::error::ToolError;
use perry_hermes_core::tool::{Tool, ToolContext, ToolOutput};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use perry_hermes_core::util::resolve_user_path;

use super::policy::{
    cross_profile_write_message, is_internal_file_status_text, sensitive_write_path_message,
    temp_sibling,
};

const WRITE_FILE_DESCRIPTION: &str = "Write content to a file, completely replacing existing content. \
Use this instead of echo/cat heredoc in terminal. Creates parent directories automatically. \
OVERWRITES the entire file — use 'patch' for targeted edits. Auto-runs syntax checks on \
.py/.json/.yaml/.toml and other linted languages; only NEW errors introduced by this write \
are surfaced (pre-existing errors are filtered out).";

pub struct WriteFileTool;

impl WriteFileTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WriteFileTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        WRITE_FILE_DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write (will be created if it doesn't exist, overwritten if it does)"
                },
                "content": {
                    "type": "string",
                    "description": "Complete content to write to the file"
                },
                "cross_profile": {
                    "type": "boolean",
                    "default": false,
                    "description": "Opt out of the cross-profile soft guard. Defaults to false. Set true ONLY after explicit user direction to edit another Perry Hermes profile's skills/plugins/cron/memories — by default these writes are blocked with a warning because they affect a different profile than the one this session is running under."
                }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }

    fn toolset(&self) -> &'static str {
        "file"
    }

    async fn execute(
        &self,
        args: Value,
        ctx: ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("missing 'path'".into()))?;
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("missing 'content'".into()))?;

        let resolved = match resolve_user_path(path_str, &ctx.working_dir) {
            Ok(p) => p,
            Err(msg) => {
                return Ok(ToolOutput {
                    content: json!({"error": msg}).to_string(),
                });
            }
        };

        if let Some(msg) = sensitive_write_path_message(path_str, &resolved) {
            return Ok(ToolOutput {
                content: json!({"error": msg}).to_string(),
            });
        }
        let cross_profile = args
            .get("cross_profile")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !cross_profile && let Some(msg) = cross_profile_write_message(&resolved) {
            return Ok(ToolOutput {
                content: json!({"error": msg}).to_string(),
            });
        }
        if is_internal_file_status_text(content) {
            return Ok(ToolOutput {
                content: json!({
                    "error": "Refusing to write internal read_file status text as file content. Re-read the file or reconstruct the intended file contents before writing."
                })
                .to_string(),
            });
        }

        let parent = resolved
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let dirs_created = if !parent.as_os_str().is_empty() && !parent.is_dir() {
            match std::fs::create_dir_all(&parent) {
                Ok(()) => true,
                Err(e) => {
                    return Ok(ToolOutput {
                        content: json!({"error": format!("Failed to create parent dir: {e}")})
                            .to_string(),
                    });
                }
            }
        } else {
            false
        };

        let tmp = match temp_sibling(&resolved) {
            Ok(p) => p,
            Err(msg) => {
                return Ok(ToolOutput {
                    content: json!({"error": msg}).to_string(),
                });
            }
        };
        if let Err(e) = std::fs::write(&tmp, content.as_bytes()) {
            let _ = std::fs::remove_file(&tmp);
            return Ok(ToolOutput {
                content: json!({"error": format!("Failed to write temp file: {e}")}).to_string(),
            });
        }
        if let Err(e) = std::fs::rename(&tmp, &resolved) {
            let _ = std::fs::remove_file(&tmp);
            return Ok(ToolOutput {
                content: json!({"error": format!("Atomic rename failed: {e}")}).to_string(),
            });
        }

        let bytes_written = content.len() as i64;
        let canonical = std::fs::canonicalize(&resolved)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| resolved.to_string_lossy().into_owned());
        Ok(ToolOutput {
            content: json!({
                "bytes_written": bytes_written,
                "dirs_created": dirs_created,
                "resolved_path": canonical,
                "files_modified": [canonical.clone()],
            })
            .to_string(),
        })
    }
}
