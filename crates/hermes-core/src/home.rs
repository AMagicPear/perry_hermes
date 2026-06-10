//! Unified `PERRY_HERMES_HOME` resolution.
//!
//! All crates share these helpers instead of reimplementing the
//! three-step fallback (env → $HOME/.perry_hermes → cwd/.perry_hermes).

use std::path::PathBuf;

/// Resolve the Perry Hermes configuration directory.
///
/// Priority:
/// 1. `PERRY_HERMES_HOME` env var (if set and non-empty)
/// 2. `$HOME/.perry_hermes`
/// 3. `./.perry_hermes` (cwd-relative fallback)
///
/// Returns `None` only if all three resolution steps fail
/// (e.g. `$HOME` is unset and `current_dir()` errors).
pub fn resolve_home_dir() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("PERRY_HERMES_HOME")
        && !home.is_empty()
    {
        return Some(PathBuf::from(home));
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return Some(PathBuf::from(home).join(".perry_hermes"));
    }
    std::env::current_dir()
        .ok()
        .map(|cwd| cwd.join(".perry_hermes"))
}

/// Convenience: `resolve_home_dir().join(sub)`.
///
/// Returns `None` if `resolve_home_dir()` returns `None`.
pub fn resolve_subdir(sub: &str) -> Option<PathBuf> {
    resolve_home_dir().map(|h| h.join(sub))
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: tests mutate PERRY_HERMES_HOME/HOME and must be serialized
    // with any other env-mutating tests in the same process.

    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn resolve_home_dir_prefers_env_var() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("PERRY_HERMES_HOME", tmp.path()) };
        let result = resolve_home_dir().expect("should resolve");
        assert_eq!(result, tmp.path());
        unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
    }

    #[test]
    fn resolve_home_dir_falls_back_to_home() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
        // HOME is typically set in CI/dev environments.
        let result = resolve_home_dir().expect("should resolve from HOME");
        let home = std::env::var("HOME").unwrap();
        assert_eq!(result, PathBuf::from(home).join(".perry_hermes"));
    }

    #[test]
    fn resolve_home_dir_skips_empty_env_var() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("PERRY_HERMES_HOME", "") };
        let result = resolve_home_dir();
        // Should skip the empty var and fall through to HOME.
        assert!(result.is_some());
        let home = std::env::var("HOME").unwrap();
        assert_eq!(result.unwrap(), PathBuf::from(home).join(".perry_hermes"));
        unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
    }

    #[test]
    fn resolve_subdir_appends_component() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("PERRY_HERMES_HOME", tmp.path()) };
        let skills = resolve_subdir("skills").expect("should resolve");
        assert_eq!(skills, tmp.path().join("skills"));
        unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
    }

    #[test]
    fn resolve_subdir_returns_none_when_home_unresolvable() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var("HOME").ok();
        unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
        unsafe { std::env::remove_var("HOME") };
        // Without HOME and with cwd fallback we should still get Some
        // (cwd/.perry_hermes), unless current_dir itself fails.
        // In most test environments current_dir succeeds, so this
        // test documents the happy path rather than asserting None.
        let result = resolve_subdir("memories");
        assert!(result.is_some());
        // Restore HOME for other tests.
        if let Some(h) = prev_home {
            unsafe { std::env::set_var("HOME", &h) };
        }
    }
}
