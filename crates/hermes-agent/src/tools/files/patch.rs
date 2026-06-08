//! `PatchTool` — targeted edits and V4A-style multi-file patches.
//!
//! Model contract is aligned with Python hermes-agent's `patch_tool`:
//! the same parameter names, the same `mode` enum, and a compatible
//! result envelope. The schema declares `mode` as the only required
//! field; the rest are mode-specific. The Rust runtime supports two
//! modes:
//!
//! - `replace` — a unique-string find-and-replace on a single file.
//! - `patch` — a V4A patch body with `*** Add File` /
//!   `*** Update File` / `*** Delete File` / `*** Move File` operations.

use std::path::PathBuf;

use async_trait::async_trait;
use perry_hermes_core::error::ToolError;
use perry_hermes_core::tool::{Tool, ToolContext, ToolOutput};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::tools::support::path::resolve_user_path;

use super::policy::{
    cross_profile_write_message, is_internal_file_status_text, sensitive_write_path_message,
    temp_sibling,
};

const PATCH_DESCRIPTION: &str = "Apply a targeted edit to a file, or a V4A multi-file patch. \
Prefer this over wholesale rewrites via write_file. Two modes:\n\
- 'replace': unique-string find-and-replace on a single file. Requires path + old_string + new_string. \
Set replace_all=true to replace every occurrence; otherwise the tool rejects ambiguous matches.\n\
- 'patch': a V4A patch body that can add, update, delete, and move files in one call. Requires patch. \
Path is unused in this mode.\n\
Set cross_profile=true to opt out of the soft guard that blocks writes to other Perry Hermes profiles.";

pub struct PatchTool;

impl PatchTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PatchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for PatchTool {
    fn name(&self) -> &str {
        "patch"
    }

    fn description(&self) -> &str {
        PATCH_DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "mode": {
                    "type": "string",
                    "enum": ["replace", "patch"],
                    "description": "Edit mode. 'replace' for targeted find-and-replace; 'patch' for a V4A multi-file patch."
                },
                "path": {
                    "type": "string",
                    "description": "File to edit. Required for mode='replace'."
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact text to replace. Required for mode='replace'."
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text. Required for mode='replace'."
                },
                "replace_all": {
                    "type": "boolean",
                    "default": false,
                    "description": "Replace every occurrence of old_string. mode='replace' only. Default false."
                },
                "patch": {
                    "type": "string",
                    "description": "V4A patch body. Required for mode='patch'."
                },
                "cross_profile": {
                    "type": "boolean",
                    "default": false,
                    "description": "Opt out of the cross-profile soft guard. Same semantics as write_file."
                }
            },
            "required": ["mode"],
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
        let mode = match args.get("mode").and_then(|v| v.as_str()) {
            Some(m) => m,
            None => {
                return Ok(ToolOutput {
                    content: json!({"error": "missing 'mode'"}).to_string(),
                })
            }
        };
        match mode {
            "replace" => Ok(replace_mode(&args, &ctx)?),
            "patch" => Ok(patch_mode(&args, &ctx)?),
            other => Ok(ToolOutput {
                content: json!({
                    "error": format!("Unknown patch mode '{other}'. Expected 'replace' or 'patch'.")
                })
                .to_string(),
            }),
        }
    }
}

fn replace_mode(args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
    let path_str = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return Ok(ToolOutput {
                content: json!({"error": "mode='replace' requires 'path'"}).to_string(),
            })
        }
    };
    let old_string = match args.get("old_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return Ok(ToolOutput {
                content: json!({"error": "mode='replace' requires 'old_string'"}).to_string(),
            })
        }
    };
    let new_string = match args.get("new_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return Ok(ToolOutput {
                content: json!({"error": "mode='replace' requires 'new_string'"}).to_string(),
            })
        }
    };
    let replace_all = args
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let cross_profile = args
        .get("cross_profile")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if is_internal_file_status_text(new_string) {
        return Ok(ToolOutput {
            content: json!({
                "error": "Refusing to write internal read_file status text as file content."
            })
            .to_string(),
        });
    }

    let resolved = match resolve_user_path(path_str, &ctx.working_dir) {
        Ok(p) => p,
        Err(msg) => {
            return Ok(ToolOutput {
                content: json!({"error": msg}).to_string(),
            })
        }
    };
    if let Some(msg) = sensitive_write_path_message(path_str, &resolved) {
        return Ok(ToolOutput {
            content: json!({"error": msg}).to_string(),
        });
    }
    if !cross_profile {
        if let Some(msg) = cross_profile_write_message(&resolved) {
            return Ok(ToolOutput {
                content: json!({"error": msg}).to_string(),
            });
        }
    }

    let original = match std::fs::read_to_string(&resolved) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ToolOutput {
                content: json!({
                    "error": format!("File not found: {path_str}")
                })
                .to_string(),
            });
        }
        Err(e) => {
            return Ok(ToolOutput {
                content: json!({"error": format!("read failed: {e}")}).to_string(),
            });
        }
    };

    let occurrences: usize = if old_string.is_empty() {
        0
    } else {
        original.match_indices(old_string).count()
    };
    if occurrences == 0 {
        return Ok(ToolOutput {
            content: json!({
                "error": "old_string not found in file. Re-read the file to confirm current contents.",
                "_hint": "old_string must match exactly including whitespace and newlines."
            })
            .to_string(),
        });
    }
    if occurrences > 1 && !replace_all {
        return Ok(ToolOutput {
            content: json!({
                "error": format!(
                    "old_string matches multiple ({occurrences}) locations. Pass replace_all=true to replace all, or include more surrounding context to make the match unique."
                )
            })
            .to_string(),
        });
    }

    let updated = if replace_all {
        original.replace(old_string, new_string)
    } else {
        original.replacen(old_string, new_string, 1)
    };

    let diff = render_diff(&original, &updated);
    let tmp = match temp_sibling(&resolved) {
        Ok(p) => p,
        Err(msg) => {
            return Ok(ToolOutput {
                content: json!({"error": msg}).to_string(),
            })
        }
    };
    if let Err(e) = std::fs::write(&tmp, updated.as_bytes()) {
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
    let canonical = std::fs::canonicalize(&resolved)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| resolved.to_string_lossy().into_owned());
    Ok(ToolOutput {
        content: json!({
            "files_modified": [canonical],
            "resolved_path": canonical,
            "diff": diff,
            "replace_all": replace_all,
        })
        .to_string(),
    })
}

fn patch_mode(args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
    let patch_body = match args.get("patch").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return Ok(ToolOutput {
                content: json!({"error": "mode='patch' requires 'patch'"}).to_string(),
            })
        }
    };
    let cross_profile = args
        .get("cross_profile")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let ops = match parse_v4a(patch_body) {
        Ok(ops) => ops,
        Err(e) => {
            return Ok(ToolOutput {
                content: json!({"error": e}).to_string(),
            })
        }
    };

    let mut results: Vec<Value> = Vec::new();
    let mut files_modified: Vec<String> = Vec::new();
    for op in ops {
        match apply_v4a_op(&op, ctx, cross_profile) {
            Ok(path) => {
                let canonical = path
                    .map(|p| {
                        std::fs::canonicalize(&p)
                            .map(|c| c.to_string_lossy().into_owned())
                            .unwrap_or_else(|_| p.to_string_lossy().into_owned())
                    })
                    .unwrap_or_default();
                if !canonical.is_empty() && !files_modified.contains(&canonical) {
                    files_modified.push(canonical.clone());
                }
                results.push(json!({
                    "op": op.kind.kind_str(),
                    "path": op.path,
                    "status": "applied",
                }));
            }
            Err(msg) => {
                results.push(json!({
                    "op": op.kind.kind_str(),
                    "path": op.path,
                    "status": "rejected",
                    "reason": msg,
                }));
            }
        }
    }
    let success = results.iter().all(|r| r["status"] == "applied");
    Ok(ToolOutput {
        content: json!({
            "success": success,
            "files_modified": files_modified,
            "results": results,
        })
        .to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OpKind {
    Add,
    Update,
    Delete,
    Move,
}

impl OpKind {
    fn kind_str(&self) -> &'static str {
        match self {
            OpKind::Add => "add",
            OpKind::Update => "update",
            OpKind::Delete => "delete",
            OpKind::Move => "move",
        }
    }
}

#[derive(Debug, Clone)]
struct V4aOp {
    kind: OpKind,
    path: String,
    /// For Update: the body of the file (already rendered back into a single string).
    /// For Add: same.
    /// For Delete/Move: unused.
    body: String,
    /// For Move: the destination path. For others: None.
    move_to: Option<String>,
}

fn parse_v4a(body: &str) -> Result<Vec<V4aOp>, String> {
    let mut lines = body.lines();
    let header = lines.next().ok_or_else(|| "empty patch body".to_string())?;
    if !header.trim_start().starts_with("*** Begin Patch") {
        return Err("patch must start with '*** Begin Patch'".to_string());
    }

    let mut ops: Vec<V4aOp> = Vec::new();
    let mut current: Option<V4aOp> = None;
    let mut mode: u8 = 0; // 0=between ops, 1=expecting @@, 2=reading hunks/body

    for line in lines {
        let trimmed = line.trim_start();
        if trimmed.starts_with("*** End Patch") {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("*** Add File:") {
            if let Some(op) = current.take() {
                ops.push(op);
            }
            let path = rest.trim().to_string();
            current = Some(V4aOp {
                kind: OpKind::Add,
                path,
                body: String::new(),
                move_to: None,
            });
            mode = 2;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("*** Update File:") {
            if let Some(op) = current.take() {
                ops.push(op);
            }
            let path = rest.trim().to_string();
            current = Some(V4aOp {
                kind: OpKind::Update,
                path,
                body: String::new(),
                move_to: None,
            });
            mode = 1;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("*** Delete File:") {
            if let Some(op) = current.take() {
                ops.push(op);
            }
            let path = rest.trim().to_string();
            current = Some(V4aOp {
                kind: OpKind::Delete,
                path,
                body: String::new(),
                move_to: None,
            });
            mode = 0;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("*** Move File:") {
            if let Some(op) = current.take() {
                ops.push(op);
            }
            let path = rest.trim().to_string();
            current = Some(V4aOp {
                kind: OpKind::Move,
                path,
                body: String::new(),
                move_to: None,
            });
            mode = 0;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("*** Move to:") {
            if let Some(op) = current.as_mut() {
                if op.kind != OpKind::Move {
                    return Err("'*** Move to:' must follow a '*** Move File:'".to_string());
                }
                op.move_to = Some(rest.trim().to_string());
                continue;
            }
            return Err("'*** Move to:' must follow a '*** Move File:'".to_string());
        }

        // Body / hunk lines apply to the current op.
        let cur = match current.as_mut() {
            Some(c) => c,
            None => return Err(format!("Unexpected line in patch: {line}")),
        };
        match cur.kind {
            OpKind::Add => {
                if let Some(rest) = line.strip_prefix('+') {
                    cur.body.push_str(rest);
                    cur.body.push('\n');
                } else if line.is_empty() {
                    cur.body.push('\n');
                } else {
                    return Err("Add File body lines must start with '+'".to_string());
                }
            }
            OpKind::Update => {
                if mode == 1 {
                    if trimmed != "@@" {
                        return Err("Update File requires '@@' marker before hunks".to_string());
                    }
                    mode = 2;
                    continue;
                }
                if let Some(rest) = line.strip_prefix(' ') {
                    cur.body.push_str(rest);
                    cur.body.push('\n');
                } else if let Some(rest) = line.strip_prefix('-') {
                    // Deleted lines: skip (we're building the new file from the kept/added lines).
                    let _ = rest;
                } else if let Some(rest) = line.strip_prefix('+') {
                    cur.body.push_str(rest);
                    cur.body.push('\n');
                } else if line.is_empty() {
                    cur.body.push('\n');
                } else {
                    return Err(
                        "Update File hunk lines must start with ' ', '-', or '+'".to_string()
                    );
                }
            }
            OpKind::Delete | OpKind::Move => {
                // These ops have no body. Any non-directive line is an error.
                return Err(format!(
                    "Unexpected body line for {} op: {line}",
                    cur.kind.kind_str()
                ));
            }
        }
    }
    if let Some(op) = current.take() {
        ops.push(op);
    }
    if ops.is_empty() {
        return Err("patch body contained no operations".to_string());
    }
    Ok(ops)
}

fn apply_v4a_op(
    op: &V4aOp,
    ctx: &ToolContext,
    cross_profile: bool,
) -> Result<Option<PathBuf>, String> {
    let resolved = resolve_user_path(&op.path, &ctx.working_dir)
        .map_err(|e| format!("path resolution failed: {e}"))?;
    if let Some(msg) = sensitive_write_path_message(&op.path, &resolved) {
        return Err(msg);
    }
    if !cross_profile {
        if let Some(msg) = cross_profile_write_message(&resolved) {
            return Err(msg);
        }
    }
    match op.kind {
        OpKind::Add => {
            if let Some(parent) = resolved.parent() {
                if !parent.as_os_str().is_empty() && !parent.is_dir() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("failed to create parent dir: {e}"))?;
                }
            }
            std::fs::write(&resolved, op.body.as_bytes())
                .map_err(|e| format!("failed to write file: {e}"))?;
            Ok(Some(resolved))
        }
        OpKind::Update => {
            let tmp = temp_sibling(&resolved)?;
            std::fs::write(&tmp, op.body.as_bytes())
                .map_err(|e| format!("failed to write temp file: {e}"))?;
            std::fs::rename(&tmp, &resolved).map_err(|e| format!("atomic rename failed: {e}"))?;
            Ok(Some(resolved))
        }
        OpKind::Delete => match std::fs::remove_file(&resolved) {
            Ok(()) => Ok(Some(resolved)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Some(resolved)),
            Err(e) => Err(format!("failed to delete file: {e}")),
        },
        OpKind::Move => {
            let dest = op
                .move_to
                .as_ref()
                .ok_or_else(|| "Move op requires '*** Move to:'".to_string())?;
            let dest_path = resolve_user_path(dest, &ctx.working_dir)
                .map_err(|e| format!("destination path resolution failed: {e}"))?;
            if let Some(msg) = sensitive_write_path_message(dest, &dest_path) {
                return Err(msg);
            }
            if !cross_profile {
                if let Some(msg) = cross_profile_write_message(&dest_path) {
                    return Err(msg);
                }
            }
            if let Some(parent) = dest_path.parent() {
                if !parent.as_os_str().is_empty() && !parent.is_dir() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("failed to create destination dir: {e}"))?;
                }
            }
            std::fs::rename(&resolved, &dest_path)
                .map_err(|e| format!("failed to move file: {e}"))?;
            Ok(Some(dest_path))
        }
    }
}

/// Render a minimal unified-diff style representation of the change.
fn render_diff(original: &str, updated: &str) -> String {
    let mut out = String::new();
    let old_lines: Vec<&str> = original.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = updated.split_inclusive('\n').collect();
    out.push_str("--- before\n");
    out.push_str("+++ after\n");
    for line in &old_lines {
        out.push('-');
        out.push_str(line);
        if !line.ends_with('\n') {
            out.push('\n');
        }
    }
    for line in &new_lines {
        out.push('+');
        out.push_str(line);
        if !line.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}
