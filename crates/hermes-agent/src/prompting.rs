use std::path::PathBuf;

pub const DEFAULT_SYSTEM_PROMPT: &str =
    "You are a careful assistant with access to a `terminal` tool. \
Use it to inspect the system or run shell commands when needed. When you have enough information \
to answer, give a concise final response — do not call tools again.";

/// Resolve the local skills directory shared by system-prompt composition
/// (`compose_system_prompt`) and the runtime tool registry
/// (`tool_catalog::build_registry`).
///
/// Resolution rules:
/// 1. `HERMES_HOME` env var if set
/// 2. else `$HOME/.perry_hermes`
/// 3. append `/skills`
///
/// This resolver is intentionally side-effect free. Prompt composition should
/// not create a skills directory just because a turn was started.
pub fn resolve_skills_dir() -> Option<PathBuf> {
    let base = std::env::var_os("HERMES_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".perry_hermes")))?;
    Some(base.join("skills"))
}

pub fn compose_system_prompt(user_prompt: Option<&str>) -> Option<String> {
    let base = user_prompt.unwrap_or(DEFAULT_SYSTEM_PROMPT);
    let Some(dir) = resolve_skills_dir() else {
        return Some(base.to_string());
    };
    let skills = match hermes_skills::load_all(&dir) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("failed to scan skills dir {}: {e}", dir.display());
            Vec::new()
        }
    };
    let skills_block = hermes_skills::render_system_prompt_block(&skills);

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
        let home = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("HERMES_HOME", home.path()) };
        let dir = resolve_skills_dir().expect("skills dir should resolve");
        assert_eq!(dir.file_name().and_then(|s| s.to_str()), Some("skills"));
        unsafe { std::env::remove_var("HERMES_HOME") };
    }
}
