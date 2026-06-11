//! Shared utility functions used across tools and other modules.

use std::path::{Path, PathBuf};

/// Truncate a string to at most `max_chars` characters, keeping head and tail.
/// Inserts a truncation notice in the middle when truncation occurs.
pub fn truncate_output(s: &str, max_chars: usize) -> String {
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

/// Resolve a user-supplied path: expand `~/` and make relative paths absolute.
pub fn resolve_user_path(input: &str, working_dir: &Path) -> Result<PathBuf, String> {
    let expanded = if let Some(stripped) = input.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            PathBuf::from(home).join(stripped)
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

/// Returns `true` if `bin` is found in `PATH` (Unix only).
pub fn which(bin: &str) -> bool {
    let null = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .unwrap();
    std::process::Command::new("which")
        .arg(bin)
        .stdout(null)
        .status()
        .is_ok_and(|s| s.success())
}
