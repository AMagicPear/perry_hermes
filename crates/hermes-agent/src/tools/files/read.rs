use async_trait::async_trait;
use hermes_core::error::ToolError;
use hermes_core::tool::{Tool, ToolContext, ToolOutput};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::tools::bash::truncate_output;
use crate::tools::support::content::looks_binary;
use crate::tools::support::path::resolve_user_path;

use super::policy::{blocked_path_message, is_binary_extension, suggest_similar_files};

const READ_FILE_DESCRIPTION: &str = "Read a text file with line numbers and pagination. \
Use this instead of cat/head/tail in terminal. Output format: 'LINE_NUM|CONTENT'. \
Suggests similar filenames if not found. Use offset and limit for large files. \
Reads exceeding ~100K characters are rejected; use offset and limit to read specific \
sections of large files. NOTE: Cannot read images or binary files — use vision_analyze \
for images.";

const MAX_READ_CHARS: usize = 100_000;

pub struct ReadFileTool;

impl ReadFileTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ReadFileTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        READ_FILE_DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (absolute, relative, or ~/path)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (1-indexed, default: 1)",
                    "default": 1,
                    "minimum": 1
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read (default: 500, max: 2000)",
                    "default": 500,
                    "maximum": 2000
                }
            },
            "required": ["path"],
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
        let offset = args
            .get("offset")
            .and_then(|v| v.as_u64())
            .unwrap_or(1)
            .max(1);
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(500)
            .clamp(1, 2000);

        let resolved = match resolve_user_path(path_str, &ctx.working_dir) {
            Ok(p) => p,
            Err(msg) => {
                return Ok(ToolOutput {
                    content: json!({"error": msg}).to_string(),
                })
            }
        };

        if let Some(msg) = blocked_path_message(&resolved) {
            return Ok(ToolOutput {
                content: json!({"error": msg}).to_string(),
            });
        }

        if let Some(ext) = resolved.extension().and_then(|s| s.to_str()) {
            if is_binary_extension(ext) {
                return Ok(ToolOutput {
                    content: json!({
                        "error": format!(
                            "Cannot read binary file '{}' (.{}). Use vision_analyze for images, or terminal to inspect binary files.",
                            path_str, ext
                        )
                    })
                    .to_string(),
                });
            }
        }

        let meta = match std::fs::metadata(&resolved) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let similar = suggest_similar_files(&resolved);
                let body = json!({
                    "error": format!("File not found: {}", path_str),
                    "similar_files": similar,
                });
                return Ok(ToolOutput {
                    content: body.to_string(),
                });
            }
            Err(e) => {
                return Ok(ToolOutput {
                    content: json!({"error": format!("stat failed: {e}")}).to_string(),
                });
            }
        };
        if !meta.is_file() {
            return Ok(ToolOutput {
                content: json!({"error": format!("{} is not a regular file", path_str)})
                    .to_string(),
            });
        }
        let file_size = meta.len() as i64;

        let raw = match std::fs::read(&resolved) {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolOutput {
                    content: json!({"error": format!("read failed: {e}")}).to_string(),
                });
            }
        };

        if looks_binary(&raw) {
            return Ok(ToolOutput {
                content: json!({
                    "error": "Binary file — cannot display as text. Use appropriate tools to handle this file type."
                })
                .to_string(),
            });
        }

        let text = String::from_utf8_lossy(&raw);
        let text = if offset == 1 {
            text.strip_prefix('\u{feff}').unwrap_or(&text).to_string()
        } else {
            text.into_owned()
        };

        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let total_lines = lines.len() as i64;
        let start = (offset as usize).saturating_sub(1);
        let end = (start + limit as usize).min(lines.len());
        let window: String = if start < lines.len() {
            lines[start..end].join("")
        } else {
            String::new()
        };

        let width = (offset + limit).to_string().len();
        let numbered: String = window
            .split_inclusive('\n')
            .enumerate()
            .map(|(i, line)| {
                let n = offset as usize + i;
                format!("{:>width$}|{line}", n, width = width)
            })
            .collect();

        let truncated_chars = numbered.chars().count() > MAX_READ_CHARS;
        let truncated_lines = total_lines > end as i64;
        let hint = if truncated_lines {
            Some(format!(
                "Use offset={} to continue reading (showing {}-{} of {} lines)",
                end + 1,
                offset,
                end,
                total_lines
            ))
        } else {
            None
        };
        let mut body = json!({
            "content": numbered,
            "total_lines": total_lines,
            "file_size": file_size,
            "truncated": truncated_lines,
            "_hint": hint,
        });
        if truncated_chars {
            let head = truncate_output(&numbered, MAX_READ_CHARS);
            body["content"] = Value::String(head);
            body["truncated"] = Value::Bool(true);
            body["_hint"] = Value::String(format!(
                "Read truncated at ~{} chars; use offset+limit to read specific sections",
                MAX_READ_CHARS
            ));
        }
        Ok(ToolOutput {
            content: body.to_string(),
        })
    }
}
