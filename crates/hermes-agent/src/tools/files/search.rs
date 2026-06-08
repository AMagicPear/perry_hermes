//! `SearchFilesTool` — content and file search.
//!
//! Model contract is aligned with Python hermes-agent's `search_files_tool`:
//! the same parameter names, the same `target` enum (`content` / `files`),
//! and the same `output_mode` enum (`content` / `files_only` / `count`).
//! Content search is ripgrep-backed (regex by default). When `rg` is not
//! installed, the tool falls back to a pure-Rust walk that does literal
//! substring matching — the result envelope is the same either way.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use async_trait::async_trait;
use perry_hermes_core::error::ToolError;
use perry_hermes_core::tool::{Tool, ToolContext, ToolOutput};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::tools::support::path::resolve_user_path;

use super::policy::is_binary_extension;

const SEARCH_FILES_DESCRIPTION: &str = "Search for text across files or find files by name. \
Two search modes:\n\
- target='content' (default): ripgrep-backed regex search inside files. Honors file_glob \
(e.g. '*.rs'), output_mode (content | files_only | count), and offset/limit for pagination. \
Binary files are auto-skipped. Returns matches with line, column, and the matching line content.\n\
- target='files': find files whose name matches the pattern (uses simple '*' wildcards). \
Sorted by modification time, newest first.\n\
Set path to scope the search; defaults to the current working directory. Falls back to a \
pure-Rust walk if ripgrep is not installed.";

pub struct SearchFilesTool;

impl SearchFilesTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SearchFilesTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for SearchFilesTool {
    fn name(&self) -> &str {
        "search_files"
    }

    fn description(&self) -> &str {
        SEARCH_FILES_DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Pattern to search for. Regex for target='content' (escape special chars with backslash to match literally); simple '*' wildcard for target='files'."
                },
                "target": {
                    "type": "string",
                    "enum": ["content", "files"],
                    "default": "content",
                    "description": "Search mode. 'content' greps inside files; 'files' matches filenames."
                },
                "path": {
                    "type": "string",
                    "description": "Root directory to search. Defaults to the current working directory."
                },
                "file_glob": {
                    "type": "string",
                    "description": "Optional filename pattern using simple '*' wildcards (target='content' only)."
                },
                "limit": {
                    "type": "integer",
                    "default": 50,
                    "minimum": 1,
                    "description": "Maximum number of items to return. Defaults to 50."
                },
                "offset": {
                    "type": "integer",
                    "default": 0,
                    "minimum": 0,
                    "description": "Number of items to skip for pagination. Defaults to 0."
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files_only", "count"],
                    "default": "content",
                    "description": "What to return. 'content' returns match lines; 'files_only' returns the file list; 'count' returns per-file match counts."
                },
                "context": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Number of context lines to include before and after each match. Each match entry then carries `context.before` and `context.after` arrays. Default 0 (no context)."
                }
            },
            "required": ["pattern"],
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
        let pattern = match args.get("pattern").and_then(|v| v.as_str()) {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => {
                return Ok(ToolOutput {
                    content: json!({"error": "missing 'pattern'"}).to_string(),
                });
            }
        };
        let target = args
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("content")
            .to_string();
        let raw_path = args.get("path").and_then(|v| v.as_str()).map(String::from);
        let file_glob = args
            .get("file_glob")
            .and_then(|v| v.as_str())
            .map(String::from);
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(50)
            .max(1) as usize;
        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let output_mode = args
            .get("output_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("content")
            .to_string();
        let context = args.get("context").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

        let root = match raw_path {
            Some(p) => match resolve_user_path(&p, &ctx.working_dir) {
                Ok(p) => p,
                Err(msg) => {
                    return Ok(ToolOutput {
                        content: json!({"error": msg}).to_string(),
                    });
                }
            },
            None => ctx.working_dir.clone(),
        };
        if !root.is_dir() {
            return Ok(ToolOutput {
                content:
                    json!({"error": format!("search path is not a directory: {}", root.display())})
                        .to_string(),
            });
        }

        match target.as_str() {
            "files" => Ok(files_mode(
                &root,
                &pattern,
                file_glob.as_deref(),
                limit,
                offset,
            )),
            "content" => Ok(content_mode(
                &root,
                &pattern,
                file_glob.as_deref(),
                &output_mode,
                context,
                limit,
                offset,
            )
            .await),
            other => Ok(ToolOutput {
                content: json!({
                    "error": format!("Unknown target '{other}'. Expected 'content' or 'files'.")
                })
                .to_string(),
            }),
        }
    }
}

fn files_mode(
    root: &Path,
    pattern: &str,
    file_glob: Option<&str>,
    limit: usize,
    offset: usize,
) -> ToolOutput {
    let mut all_files: Vec<PathBuf> = Vec::new();
    collect_files(root, &mut all_files, file_glob);
    let mut matched: Vec<PathBuf> = all_files
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| glob_match(pattern, n))
                .unwrap_or(false)
        })
        .collect();
    matched.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    matched.reverse();
    let total = matched.len();
    let page: Vec<String> = matched
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let truncated = total > offset + limit;
    ToolOutput {
        content: json!({
            "files": page,
            "total": total,
            "truncated": truncated,
        })
        .to_string(),
    }
}

async fn content_mode(
    root: &Path,
    pattern: &str,
    file_glob: Option<&str>,
    output_mode: &str,
    context: usize,
    limit: usize,
    offset: usize,
) -> ToolOutput {
    if rg_available() {
        return rg_content_search(
            root,
            pattern,
            file_glob,
            output_mode,
            context,
            limit,
            offset,
        )
        .await;
    }
    content_mode_walk(root, pattern, file_glob, output_mode, limit, offset)
}

fn content_mode_walk(
    root: &Path,
    pattern: &str,
    file_glob: Option<&str>,
    output_mode: &str,
    limit: usize,
    offset: usize,
) -> ToolOutput {
    let mut all_files: Vec<PathBuf> = Vec::new();
    collect_files(root, &mut all_files, file_glob);
    let mut all_matches: Vec<Value> = Vec::new();
    let mut per_file: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut files_with_matches: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    for path in all_files {
        if let Some(ext) = path.extension().and_then(|s| s.to_str())
            && is_binary_extension(ext)
        {
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let path_str = path.to_string_lossy().into_owned();
        for (line_no, line) in raw.lines().enumerate() {
            let mut start = 0usize;
            while let Some(pos) = line[start..].find(pattern) {
                let column = start + pos + 1;
                all_matches.push(json!({
                    "path": path_str,
                    "line": line_no + 1,
                    "column": column,
                    "content": line,
                }));
                *per_file.entry(path_str.clone()).or_insert(0) += 1;
                files_with_matches.insert(path_str.clone());
                start += pos + pattern.len();
                if pattern.is_empty() {
                    break;
                }
            }
        }
    }
    let total = all_matches.len();

    let payload = match output_mode {
        "files_only" => {
            // In files_only mode the offset/limit window is on underlying
            // matches, not on the file list — return every file that
            // contributed at least one match and report total/truncated in
            // match units.
            let page: Vec<String> = files_with_matches.into_iter().collect();
            json!({
                "files": page,
                "total": total,
                "truncated": total > limit,
            })
        }
        "count" => {
            let mut counts: Vec<(String, usize)> = per_file.into_iter().collect();
            counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let total_files = counts.len();
            let page: Vec<Value> = counts
                .into_iter()
                .skip(offset)
                .take(limit)
                .map(|(path, count)| json!({"path": path, "count": count}))
                .collect();
            json!({
                "counts": page,
                "total": total,
                "truncated": total_files > offset + limit,
            })
        }
        _ => {
            // content mode
            let page: Vec<Value> = all_matches.into_iter().skip(offset).take(limit).collect();
            json!({
                "matches": page,
                "total": total,
                "truncated": total > offset + limit,
            })
        }
    };
    ToolOutput {
        content: payload.to_string(),
    }
}

fn collect_files(root: &Path, out: &mut Vec<PathBuf>, file_glob: Option<&str>) {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            // Skip common VCS / target / hidden noise so first-pass search is useful.
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if matches!(
                name,
                ".git" | "target" | "node_modules" | ".venv" | "__pycache__"
            ) {
                continue;
            }
            collect_files(&path, out, file_glob);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if let Some(glob) = file_glob {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if !glob_match(glob, name) {
                continue;
            }
        }
        out.push(path);
    }
}

/// Simple `*` wildcard matcher. Only `*` is interpreted; everything else
/// is a literal character.
fn glob_match(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let pat: Vec<char> = pattern.chars().collect();
    let nam: Vec<char> = name.chars().collect();
    let (pn, nn) = (pat.len(), nam.len());
    let mut dp = vec![vec![false; nn + 1]; pn + 1];
    dp[0][0] = true;
    for i in 1..=pn {
        if pat[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }
    for i in 1..=pn {
        for j in 1..=nn {
            if pat[i - 1] == '*' {
                dp[i][j] = dp[i - 1][j] || dp[i][j - 1];
            } else if pat[i - 1] == nam[j - 1] {
                dp[i][j] = dp[i - 1][j - 1];
            }
        }
    }
    dp[pn][nn]
}

/// Probe for `rg` once per process and cache the result. Uses
/// `std::process::Command` rather than the async variant so the probe is
/// a pure synchronous `exec` of a no-op (`--version`) — no reason to take
/// a hot async path through it.
fn rg_available() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| {
        std::process::Command::new("rg")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// Ripgrep backend for `target='content'`. Spawns `rg --json` and shapes
/// the result envelope the same way the walk fallback does, so callers
/// see a consistent schema regardless of which backend ran.
async fn rg_content_search(
    root: &Path,
    pattern: &str,
    file_glob: Option<&str>,
    output_mode: &str,
    context: usize,
    limit: usize,
    offset: usize,
) -> ToolOutput {
    let mut cmd = tokio::process::Command::new("rg");
    cmd.arg("--json")
        .arg("--no-heading")
        .arg("-e")
        .arg(pattern)
        .arg(root);
    if let Some(g) = file_glob {
        cmd.arg("-g").arg(g);
    }
    if context > 0 {
        cmd.arg("-C").arg(context.to_string());
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let output = match cmd.output().await {
        Ok(o) => o,
        Err(e) => {
            return ToolOutput {
                content: json!({"error": format!("rg spawn failed: {e}")}).to_string(),
            };
        }
    };
    // rg exit codes: 0 = matches, 1 = no matches, ≥2 = error.
    let exit = output.status.code().unwrap_or(-1);
    if exit >= 2 {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return ToolOutput {
            content: json!({"error": format!("rg error: {}", stderr.trim())}).to_string(),
        };
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut all_matches: Vec<Value> = Vec::new();
    // When `context` is set, rg emits a `context` event for each line
    // surrounding a match. We attribute them to the nearest match: lines
    // before the next match become that match's `context.before`; the
    // N lines immediately after a match become its `context.after`.
    let mut pending_before: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let mut last_match_idx: Option<usize> = None;
    let mut after_remaining: usize = 0;
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match v["type"].as_str() {
            Some("context") => {
                let content = v["data"]["lines"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .trim_end_matches('\n')
                    .to_string();
                if after_remaining > 0 {
                    // Line is also within the previous match's after window.
                    if let Some(idx) = last_match_idx {
                        let after = all_matches[idx]["context"]["after"]
                            .as_array_mut()
                            .expect("context.after array");
                        after.push(Value::String(content.clone()));
                    }
                    after_remaining -= 1;
                }
                // Also keep it in the rolling "before" buffer for the next
                // match. When two matches are within 2N lines of each
                // other, the lines between them appear in both arrays —
                // that's the standard grep -C semantics.
                pending_before.push_back(content);
                while pending_before.len() > context {
                    pending_before.pop_front();
                }
            }
            Some("match") => {
                let path = v["data"]["path"]["text"].as_str().unwrap_or("").to_string();
                let line_no = v["data"]["line_number"].as_u64().unwrap_or(0);
                let raw_line = v["data"]["lines"]["text"].as_str().unwrap_or("");
                let content = raw_line.trim_end_matches('\n').to_string();
                let column = v["data"]["submatches"][0]["start"].as_u64().unwrap_or(0) as usize + 1;
                let before: Vec<Value> = pending_before.drain(..).map(Value::String).collect();
                all_matches.push(json!({
                    "path": path,
                    "line": line_no,
                    "column": column,
                    "content": content,
                    "context": {
                        "before": before,
                        "after": Vec::<Value>::new(),
                    }
                }));
                last_match_idx = Some(all_matches.len() - 1);
                after_remaining = context;
            }
            _ => {}
        }
    }
    let total = all_matches.len();

    let payload = match output_mode {
        "files_only" => {
            // In files_only mode the offset/limit window is on underlying
            // matches, not on the file list — return every file that
            // contributed at least one match and report total/truncated in
            // match units.
            let mut files: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for m in &all_matches {
                if let Some(p) = m["path"].as_str() {
                    files.insert(p.to_string());
                }
            }
            let page: Vec<String> = files.into_iter().collect();
            json!({
                "files": page,
                "total": total,
                "truncated": total > limit,
            })
        }
        "count" => {
            let mut counts: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            for m in &all_matches {
                if let Some(p) = m["path"].as_str() {
                    *counts.entry(p.to_string()).or_insert(0) += 1;
                }
            }
            let mut counts: Vec<(String, usize)> = counts.into_iter().collect();
            counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let total_files = counts.len();
            let page: Vec<Value> = counts
                .into_iter()
                .skip(offset)
                .take(limit)
                .map(|(path, count)| json!({"path": path, "count": count}))
                .collect();
            json!({
                "counts": page,
                "total": total,
                "truncated": total_files > offset + limit,
            })
        }
        _ => {
            let page: Vec<Value> = all_matches.into_iter().skip(offset).take(limit).collect();
            json!({
                "matches": page,
                "total": total,
                "truncated": total > offset + limit,
            })
        }
    };
    ToolOutput {
        content: payload.to_string(),
    }
}
