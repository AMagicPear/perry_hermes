use std::path::{Path, PathBuf};

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
