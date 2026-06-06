//! `ReadFileTool` / `WriteFileTool` — text file I/O for the LLM.
//!
//! Public contract (name, schema, toolset) is aligned with Python's
//! `read_file_tool` / `write_file_tool` so prompts and tool calls stay
//! portable.

use std::path::PathBuf;

use async_trait::async_trait;
use hermes_core::error::ToolError;
use hermes_core::tool::{Tool, ToolContext, ToolOutput};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::tools::bash::truncate_output;

// ---------------------------------------------------------------------------
// ReadFileTool
// ---------------------------------------------------------------------------

/// Description for the `read_file` tool, copied from Python's
/// `READ_FILE_SCHEMA["description"]`.
const READ_FILE_DESCRIPTION: &str = "Read a text file with line numbers and pagination. \
Use this instead of cat/head/tail in terminal. Output format: 'LINE_NUM|CONTENT'. \
Suggests similar filenames if not found. Use offset and limit for large files. \
Reads exceeding ~100K characters are rejected; use offset and limit to read specific \
sections of large files. NOTE: Cannot read images or binary files — use vision_analyze \
for images.";

/// Maximum characters of formatted content returned to the model.
const MAX_READ_CHARS: usize = 100_000;
const READ_DEDUP_STATUS_MESSAGE: &str =
    "File unchanged since last read. The content from the earlier read_file result in this conversation is still current — refer to that instead of re-reading.";

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
        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1).max(1);
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(500)
            .clamp(1, 2000);

        let resolved = match resolve_user_path(path_str, &ctx.working_dir) {
            Ok(p) => p,
            Err(msg) => return Ok(ToolOutput { content: json!({"error": msg}).to_string() }),
        };

        if let Some(msg) = blocked_path_message(&resolved) {
            return Ok(ToolOutput { content: json!({"error": msg}).to_string() });
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

        // Read the file, capturing metadata. Distinguish "not a file" from
        // "not found" so the LLM gets a useful error.
        let meta = match std::fs::metadata(&resolved) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let similar = suggest_similar_files(&resolved);
                let body = json!({
                    "error": format!("File not found: {}", path_str),
                    "similar_files": similar,
                });
                return Ok(ToolOutput { content: body.to_string() });
            }
            Err(e) => {
                return Ok(ToolOutput {
                    content: json!({"error": format!("stat failed: {e}")}).to_string(),
                });
            }
        };
        if !meta.is_file() {
            return Ok(ToolOutput {
                content: json!({"error": format!("{} is not a regular file", path_str)}).to_string(),
            });
        }
        let file_size = meta.len() as i64;

        // Read the full file (Rust tools do not have a separate cat backend;
        // keeping it simple).
        let raw = match std::fs::read(&resolved) {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolOutput {
                    content: json!({"error": format!("read failed: {e}")}).to_string(),
                });
            }
        };

        // Binary detection: >5% non-printable in the first 1KB.
        if looks_binary(&raw) {
            return Ok(ToolOutput {
                content: json!({
                    "error": "Binary file — cannot display as text. Use appropriate tools to handle this file type."
                })
                .to_string(),
            });
        }

        // Decode UTF-8 (lossy is fine for line numbering).
        let text = String::from_utf8_lossy(&raw);
        let text = if offset == 1 {
            text.strip_prefix('\u{feff}').unwrap_or(&text).to_string()
        } else {
            text.into_owned()
        };

        // 1-indexed slice.
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let total_lines = lines.len() as i64;
        let start = (offset as usize).saturating_sub(1);
        let end = (start + limit as usize).min(lines.len());
        let window: String = if start < lines.len() {
            lines[start..end].join("")
        } else {
            String::new()
        };

        // Number each line.
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
        Ok(ToolOutput { content: body.to_string() })
    }
}

// ---------------------------------------------------------------------------
// WriteFileTool
// ---------------------------------------------------------------------------

/// Description for the `write_file` tool, copied from Python's
/// `WRITE_FILE_SCHEMA["description"]`.
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
                    "description": "Opt out of the cross-profile soft guard. Defaults to false. Set true ONLY after explicit user direction to edit another Hermes profile's skills/plugins/cron/memories — by default these writes are blocked with a warning because they affect a different profile than the one this session is running under."
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
        let _cross_profile = args
            .get("cross_profile")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let resolved = match resolve_user_path(path_str, &ctx.working_dir) {
            Ok(p) => p,
            Err(msg) => return Ok(ToolOutput { content: json!({"error": msg}).to_string() }),
        };

        if let Some(msg) = sensitive_write_path_message(path_str, &resolved) {
            return Ok(ToolOutput { content: json!({"error": msg}).to_string() });
        }
        let cross_profile = args
            .get("cross_profile")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !cross_profile {
            if let Some(msg) = cross_profile_write_message(&resolved) {
                return Ok(ToolOutput { content: json!({"error": msg}).to_string() });
            }
        }
        if is_internal_file_status_text(content) {
            return Ok(ToolOutput {
                content: json!({
                    "error": "Refusing to write internal read_file status text as file content. Re-read the file or reconstruct the intended file contents before writing."
                })
                .to_string(),
            });
        }

        // Create parent directories as needed.
        let parent = resolved.parent().map(|p| p.to_path_buf()).unwrap_or_default();
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

        // Atomic write: temp file in the same directory, then rename.
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
            })
            .to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn resolve_user_path(input: &str, working_dir: &std::path::Path) -> Result<PathBuf, String> {
    let expanded = if let Some(stripped) = input.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            std::path::PathBuf::from(home).join(stripped)
        } else {
            return Err("~ expansion requested but $HOME is not set".to_string());
        }
    } else {
        PathBuf::from(input)
    };
    if expanded.is_absolute() {
        Ok(expanded)
    } else {
        Ok(working_dir.join(expanded))
    }
}

fn sensitive_write_path_message(original: &str, resolved: &std::path::Path) -> Option<String> {
    let normalized = normalize_path_string(original);
    let resolved_str = resolved.to_string_lossy();
    let exact_blocked = ["/etc/passwd", "/etc/shadow"];
    let prefix_blocked = ["/bin", "/sbin", "/usr/bin", "/usr/sbin", "/var/run"];

    if exact_blocked
        .iter()
        .any(|entry| normalized == *entry || resolved_str == *entry)
        || prefix_blocked
            .iter()
            .any(|prefix| has_path_prefix(&normalized, prefix) || has_path_prefix(&resolved_str, prefix))
    {
        return Some(format!(
            "Refusing to write to sensitive system path: {original}\nUse the terminal tool with sudo if you need to modify system files."
        ));
    }

    let hermes_config = hermes_config_path();
    if normalized == hermes_config || resolved_str == hermes_config {
        return Some(format!(
            "Refusing to write to Hermes config file: {original}\nAgent cannot modify security-sensitive configuration. Edit ~/.perry_hermes/config.yaml directly or use 'hermes config' instead."
        ));
    }
    None
}

fn cross_profile_write_message(resolved: &std::path::Path) -> Option<String> {
    let path = resolved.to_string_lossy();
    let marker = path.find("/profiles/")?;
    let rest = &path[marker + "/profiles/".len()..];
    let mut parts = rest.split('/');
    let profile = parts.next()?;
    let remainder: Vec<&str> = parts.collect();
    if remainder.len() < 2 {
        return None;
    }
    let scoped_dir = remainder[0];
    if !matches!(scoped_dir, "skills" | "plugins" | "cron" | "memories") {
        return None;
    }

    let active_profile = std::env::var("HERMES_PROFILE")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(current_profile_from_hermes_home);
    match active_profile {
        Some(active) if active == profile => None,
        _ => Some(format!(
            "Refusing cross-profile write to Hermes {scoped_dir} for profile '{profile}'. Pass cross_profile=true only after explicit user direction."
        )),
    }
}

fn current_profile_from_hermes_home() -> Option<String> {
    let home = std::env::var_os("HERMES_HOME")?;
    let path = PathBuf::from(home);
    let parent = path.parent()?;
    if parent.file_name().and_then(|s| s.to_str()) == Some("profiles") {
        return path.file_name().and_then(|s| s.to_str()).map(|s| s.to_string());
    }
    None
}

fn hermes_config_path() -> String {
    let base = std::env::var_os("HERMES_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".perry_hermes")))
        .unwrap_or_else(|| PathBuf::from("~/.perry_hermes"));
    base.join("config.yaml").to_string_lossy().into_owned()
}

fn normalize_path_string(input: &str) -> String {
    if let Some(stripped) = input.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(stripped).to_string_lossy().into_owned();
        }
    }
    input.to_string()
}

fn has_path_prefix(path: &str, prefix: &str) -> bool {
    path == prefix || path.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('/'))
}

fn is_internal_file_status_text(content: &str) -> bool {
    let stripped = content.trim();
    if stripped.is_empty() {
        return false;
    }
    if stripped == READ_DEDUP_STATUS_MESSAGE {
        return true;
    }
    stripped.contains(READ_DEDUP_STATUS_MESSAGE)
        && stripped.len() <= 2 * READ_DEDUP_STATUS_MESSAGE.len()
}

fn blocked_path_message(path: &std::path::Path) -> Option<String> {
    let literal = path.to_string_lossy();
    let literal_blocked = [
        "/dev/zero", "/dev/urandom", "/dev/random", "/dev/full",
        "/dev/stdin", "/dev/tty", "/dev/console",
        "/dev/stdout", "/dev/stderr",
        "/dev/fd/0", "/dev/fd/1", "/dev/fd/2",
    ];
    for entry in literal_blocked {
        if literal == entry {
            return Some(format!(
                "Cannot read '{}': this is a device file that would block or produce infinite output.",
                literal
            ));
        }
    }
    let canonical = match std::fs::canonicalize(path) {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => return None,
    };
    for entry in literal_blocked {
        if canonical == entry {
            return Some(format!(
                "Cannot read '{}': this is a device file that would block or produce infinite output.",
                literal
            ));
        }
    }
    // /proc/*/fd/{0,1,2} and /proc/*/{environ,cmdline,maps}.
    if canonical.starts_with("/proc/") {
        for tail in ["/fd/0", "/fd/1", "/fd/2", "/environ", "/cmdline", "/maps"] {
            if canonical.ends_with(tail) {
                return Some(format!(
                    "Cannot read '{}': this path can leak credentials or memory layout.",
                    literal
                ));
            }
        }
    }
    None
}

fn is_binary_extension(ext: &str) -> bool {
    let lower = ext.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "avif" | "ico"
            | "pdf" | "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar"
            | "exe" | "dll" | "so" | "dylib" | "class" | "pyc" | "wasm"
            | "mp4" | "mp3" | "wav" | "flac" | "ogg" | "m4a"
            | "ttf" | "otf" | "woff" | "woff2" | "eot"
            | "psd" | "ai" | "sketch" | "fig" | "blend"
            | "glb" | "gltf" | "obj" | "fbx" | "stl" | "3ds" | "dae"
            | "db" | "sqlite" | "sqlite3"
            | "bin" | "dat" | "iso" | "dmg" | "deb" | "rpm"
            | "svg"
    )
}

fn looks_binary(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(1000)];
    if sample.is_empty() {
        return false;
    }
    let non_printable = sample
        .iter()
        .filter(|b| !matches!(**b, 0x09 | 0x0A | 0x0D | 0x20..=0x7E))
        .count();
    (non_printable * 20) > sample.len() // >5%
}

fn suggest_similar_files(path: &std::path::Path) -> Vec<String> {
    let dir = match path.parent() {
        Some(d) if d.is_dir() => d,
        _ => return Vec::new(),
    };
    let target_name = match path.file_name().and_then(|s| s.to_str()) {
        Some(n) => n.to_ascii_lowercase(),
        None => return Vec::new(),
    };
    let stem = std::path::Path::new(&target_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let ext = std::path::Path::new(&target_name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut scored: Vec<(i32, String)> = Vec::new();
    for entry in entries.flatten() {
        let name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        let lname = name.to_ascii_lowercase();
        let lstem = std::path::Path::new(&lname)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let lext = std::path::Path::new(&lname)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let score = if lname == target_name {
            100
        } else if !stem.is_empty() && lstem == stem {
            90
        } else if lname.starts_with(&target_name) || target_name.starts_with(&lname) {
            70
        } else if lname.contains(&target_name) {
            60
        } else if target_name.contains(&lname) && lname.len() > 2 {
            40
        } else if !ext.is_empty() && lext == ext {
            let common: std::collections::HashSet<char> =
                target_name.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            let cand: std::collections::HashSet<char> =
                lname.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            let inter = common.intersection(&cand).count();
            let larger = common.len().max(cand.len());
            if larger > 0 && inter * 5 >= larger * 2 {
                30
            } else {
                0
            }
        } else {
            0
        };
        if score > 0 {
            scored.push((score, entry.path().to_string_lossy().into_owned()));
        }
    }
    scored.sort_by_key(|item| std::cmp::Reverse(item.0));
    scored.into_iter().take(5).map(|(_, p)| p).collect()
}

fn temp_sibling(target: &std::path::Path) -> Result<PathBuf, String> {
    let parent = target
        .parent()
        .ok_or_else(|| "write target has no parent directory".to_string())?;
    let pid = std::process::id();
    let mut tmp = parent.to_path_buf();
    let fname = match target.file_name().and_then(|s| s.to_str()) {
        Some(n) => format!(".hermes-tmp-{n}.{pid}"),
        None => format!(".hermes-tmp-{pid}"),
    };
    tmp.push(fname);
    Ok(tmp)
}
