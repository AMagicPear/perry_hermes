//! `SearchFilesTool` — content and file search.
//!
//! Model contract is aligned with Python hermes-agent's `search_files_tool`:
//! the same parameter names, the same `target` enum (`content` / `files`),
//! and the same `output_mode` enum (`content` / `files_only` / `count`).
//! Patterns are treated as literal substrings on the first pass — no regex
//! yet, but the result envelope is shaped so a regex backend can be
//! dropped in later.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use perry_hermes_core::error::ToolError;
use perry_hermes_core::tool::{Tool, ToolContext, ToolOutput};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::tools::support::path::resolve_user_path;

use super::policy::is_binary_extension;

const SEARCH_FILES_DESCRIPTION: &str = "Search for text across files or find files by name. \
Two search modes:\n\
- target='content' (default): search for a literal substring inside files. Honors file_glob \
(e.g. '*.rs'), output_mode (content | files_only | count), and offset/limit for pagination. \
Binary extensions are skipped. Returns matches with line, column, and the matching line content.\n\
- target='files': find files whose name matches the pattern (uses simple '*' wildcards). \
Sorted by modification time, newest first.\n\
Set path to scope the search; defaults to the current working directory.";

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
                    "description": "Pattern to search for. Literal substring for target='content'; simple '*' wildcard for target='files'."
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
                    "description": "Lines of surrounding context to include around each match. Not yet implemented."
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
                })
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
            .unwrap_or(500)
            .max(1) as usize;
        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let output_mode = args
            .get("output_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("content")
            .to_string();

        let root = match raw_path {
            Some(p) => match resolve_user_path(&p, &ctx.working_dir) {
                Ok(p) => p,
                Err(msg) => {
                    return Ok(ToolOutput {
                        content: json!({"error": msg}).to_string(),
                    })
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
            "files" => Ok(files_mode(&root, &pattern, limit, offset)),
            "content" => Ok(content_mode(
                &root,
                &pattern,
                file_glob.as_deref(),
                &output_mode,
                limit,
                offset,
            )),
            other => Ok(ToolOutput {
                content: json!({
                    "error": format!("Unknown target '{other}'. Expected 'content' or 'files'.")
                })
                .to_string(),
            }),
        }
    }
}

fn files_mode(root: &Path, pattern: &str, limit: usize, offset: usize) -> ToolOutput {
    let mut all_files: Vec<PathBuf> = Vec::new();
    collect_files(root, &mut all_files, None);
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

fn content_mode(
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
        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
            if is_binary_extension(ext) {
                continue;
            }
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
