use std::path::{Path, PathBuf};

pub const DEFAULT_SYSTEM_PROMPT: &str =
    "You are a careful assistant with access to a `bash` tool. \
Use it to inspect the system or run shell commands when needed. When you have enough information \
to answer, give a concise final response — do not call tools again.";

/// Resolve the local skills directory shared by system-prompt composition
/// (`compose_system_prompt`) and the runtime tool registry
/// (`tool_catalog::build_registry`). Both consumers must agree on the path or
/// the system-prompt skill index and the runtime tools would scan different
/// directories.
///
/// Resolution rules:
/// 1. `HERMES_HOME` env var if set
/// 2. else `$HOME/.perry_hermes`
/// 3. append `/skills`
/// 4. create the directory on first access when missing (best-effort; a
///    permission failure is reported as a warning, not propagated)
pub fn resolve_skills_dir() -> PathBuf {
    let base: PathBuf = std::env::var_os("HERMES_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".perry_hermes")))
        .unwrap_or_else(|| PathBuf::from(".perry_hermes"));
    let dir = base.join("skills");
    ensure_dir(&dir);
    dir
}

fn ensure_dir(dir: &Path) {
    if dir.is_dir() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!("failed to create skills dir {}: {e}", dir.display());
    }
}

pub fn compose_system_prompt(user_prompt: Option<&str>) -> Option<String> {
    let dir = resolve_skills_dir();
    let skills = match hermes_skills::load_all(&dir) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("failed to scan skills dir {}: {e}", dir.display());
            Vec::new()
        }
    };
    let skills_block = hermes_skills::render_system_prompt_block(&skills);
    let base = user_prompt.unwrap_or(DEFAULT_SYSTEM_PROMPT);

    if skills_block.is_empty() {
        Some(base.to_string())
    } else {
        Some(format!("{base}\n\n{skills_block}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_returns_a_directory_path_that_ends_in_skills() {
        // Don't mutate the user's real ~/.perry_hermes in tests; the helper
        // is allowed to be a no-op when HERMES_HOME is set, so this just
        // asserts the path shape.
        let dir = resolve_skills_dir();
        assert_eq!(dir.file_name().and_then(|s| s.to_str()), Some("skills"));
    }
}
