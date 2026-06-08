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
                    "default": "replace",
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

#[derive(Debug, Clone, Default)]
struct Hunk {
    /// Lines (without trailing newline) that must appear contiguously in
    /// the existing file. Built from ` ` and `-` hunk lines.
    search: Vec<String>,
    /// Lines that replace the matched search range. Built from ` ` and
    /// `+` hunk lines.
    replace: Vec<String>,
}

#[derive(Debug, Clone)]
struct V4aOp {
    kind: OpKind,
    path: String,
    /// For Add: the new file content (with trailing newlines preserved).
    /// For Update: unused — hunks are applied to the existing file.
    /// For Delete/Move: unused.
    body: String,
    /// For Update: one or more hunks to apply to the existing file. Each
    /// hunk is matched against the file's current content and replaced
    /// in place. Lines outside the matched range are preserved.
    hunks: Vec<Hunk>,
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
        if is_v4a_end_marker(trimmed) {
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
                hunks: Vec::new(),
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
                hunks: Vec::new(),
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
                hunks: Vec::new(),
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
                hunks: Vec::new(),
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
                if trimmed == "@@" {
                    // Start a new hunk. The first `@@` after `*** Update File:`
                    // transitions mode 1 → 2 and opens the first hunk; later
                    // `@@`s close the current hunk and open the next.
                    cur.hunks.push(Hunk::default());
                    mode = 2;
                    continue;
                }
                if mode == 1 {
                    return Err("Update File requires '@@' marker before hunks".to_string());
                }
                // Body / hunk lines apply to the current op.
                let cur_hunk = match cur.hunks.last_mut() {
                    Some(h) => h,
                    None => {
                        return Err("Update File hunk lines must follow '@@'".to_string());
                    }
                };
                if let Some(rest) = line.strip_prefix(' ') {
                    cur_hunk.search.push(rest.to_string());
                    cur_hunk.replace.push(rest.to_string());
                } else if let Some(rest) = line.strip_prefix('-') {
                    cur_hunk.search.push(rest.to_string());
                } else if let Some(rest) = line.strip_prefix('+') {
                    cur_hunk.replace.push(rest.to_string());
                } else if line.is_empty() {
                    cur_hunk.search.push(String::new());
                    cur_hunk.replace.push(String::new());
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
            if op.hunks.is_empty() {
                return Err("Update File has no hunks (missing '@@' marker)".to_string());
            }
            let original = match std::fs::read_to_string(&resolved) {
                Ok(s) => s,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(format!("Update target file not found: {}", op.path));
                }
                Err(e) => return Err(format!("read failed: {e}")),
            };
            let updated = apply_hunks_to_text(&original, &op.hunks, &op.path)?;
            let tmp = temp_sibling(&resolved)?;
            std::fs::write(&tmp, updated.as_bytes())
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

/// Apply V4A hunks to a file's text content. Lines outside the matched
/// range are preserved verbatim; only the matched range is replaced by
/// the hunk's `replace` lines. Each hunk is matched against the file
/// text as it stands after the previous hunk was applied, so multiple
/// hunks compose in order.
///
/// The text is split into lines without trailing newlines for matching,
/// then re-joined. A trailing newline is preserved if the original had
/// one, so we don't accidentally append a blank line to files that
/// weren't newline-terminated.
fn apply_hunks_to_text(
    original: &str,
    hunks: &[Hunk],
    path_for_error: &str,
) -> Result<String, String> {
    let has_trailing_newline = original.ends_with('\n');
    // split('\n') leaves an empty trailing string when the file ends
    // with '\n'; drop it so each "line" is a real line.
    let raw_lines: Vec<&str> = original.split('\n').collect();
    let mut lines: Vec<String> = if has_trailing_newline {
        raw_lines[..raw_lines.len() - 1]
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        raw_lines.iter().map(|s| s.to_string()).collect()
    };

    for (hunk_idx, hunk) in hunks.iter().enumerate() {
        if hunk.search.is_empty() {
            return Err(format!(
                "hunk #{} for '{path_for_error}' has no search context \
                 (every line is '+'); include a context (' ') or deletion ('-') line to anchor the position",
                hunk_idx + 1
            ));
        }

        // Count occurrences to detect ambiguity before we splice.
        let occurrences = count_subsequence(&lines, &hunk.search);
        if occurrences == 0 {
            return Err(format!(
                "hunk #{} for '{path_for_error}' did not apply: search text not found in file. \
                 Re-read the file to confirm current contents, or include more context lines.",
                hunk_idx + 1
            ));
        }
        if occurrences > 1 {
            return Err(format!(
                "hunk #{} for '{path_for_error}' matched {occurrences} locations. \
                 Add more context lines to make the match unique.",
                hunk_idx + 1
            ));
        }

        // Find the single match and splice.
        let pos =
            find_subsequence(&lines, &hunk.search).expect("occurrences > 0 guarantees a match");
        lines.splice(pos..pos + hunk.search.len(), hunk.replace.iter().cloned());
    }

    let mut out = lines.join("\n");
    if has_trailing_newline {
        out.push('\n');
    }
    Ok(out)
}

fn find_subsequence<T: PartialEq>(haystack: &[T], needle: &[T]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if haystack.len() < needle.len() {
        return None;
    }
    for start in 0..=haystack.len() - needle.len() {
        if &haystack[start..start + needle.len()] == needle {
            return Some(start);
        }
    }
    None
}

fn count_subsequence<T: PartialEq>(haystack: &[T], needle: &[T]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut start = 0;
    while let Some(pos) = find_subsequence(&haystack[start..], needle) {
        count += 1;
        start += pos + needle.len();
    }
    count
}

/// Accept the V4A end-of-patch marker in any of the common shapes:
/// `*** End Patch`, `*** End of Patch`, with or without a trailing
/// `[N]` index. The exact `*** End Patch` form is the design-doc
/// spelling; the others show up in documentation, tooling, and
/// pre-existing patches.
fn is_v4a_end_marker(trimmed: &str) -> bool {
    if !trimmed.starts_with("*** End") {
        return false;
    }
    // The body must contain the word "Patch" so we don't accept a stray
    // `*** End` line. `[N]` index and trailing whitespace are allowed.
    let after = trimmed["*** End".len()..].trim_start();
    after.starts_with("Patch") || after.starts_with("of Patch")
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
