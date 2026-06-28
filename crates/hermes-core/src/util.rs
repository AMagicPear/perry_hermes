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
        if let Some(home) = crate::home::user_home_dir() {
            PathBuf::from(home).join(stripped)
        } else {
            return Err("~ expansion requested but $HOME/$USERPROFILE is not set".to_string());
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

/// Returns `true` if `bin` is found in `PATH` (cross-platform).
///
/// On Unix this shells out to `which(1)`. On Windows it shells out to the
/// built-in `where.exe`. Both probes redirect their own stdout/stderr to
/// the platform's null device and treat a clean exit code as "found".
pub fn which(bin: &str) -> bool {
    #[cfg(unix)]
    {
        // /dev/null is mandated by POSIX; treat its absence as a hard error
        // rather than swallowing it, since callers depend on a definitive
        // yes/no answer.
        let null = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .expect("/dev/null must exist on Unix");
        std::process::Command::new("which")
            .arg(bin)
            .stdout(null)
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(windows)]
    {
        // `where.exe` ships with every supported Windows version. Probe
        // both the bare name and the `.exe`-suffixed name because `where`
        // does not consult PATHEXT the way CreateProcess does.
        let probe = |candidate: &str| -> bool {
            std::process::Command::new("where")
                .arg(candidate)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        if probe(bin) {
            return true;
        }
        if !bin.to_ascii_lowercase().ends_with(".exe")
            && !bin.to_ascii_lowercase().ends_with(".bat")
            && !bin.to_ascii_lowercase().ends_with(".cmd")
        {
            return probe(&format!("{bin}.exe"));
        }
        false
    }
}

/// Pick the right shell + argument list to run `command` on the current
/// platform.
///
/// - **Windows**: prefers PowerShell Core (`pwsh`), then Windows
///   PowerShell (`powershell`), then `cmd.exe` as a last resort. PowerShell
///   is launched with `-NoProfile` so a user's `$PROFILE` (which can be
///   slow, prompt the user, or even block on missing modules) never runs.
/// - **Unix**: prefers `zsh`, then `bash`, then POSIX `sh`. `sh` is
///   always present, so this never fails to resolve.
///
/// The returned tuple is `(program, args_before_command)`. Callers pass
/// `command` as the final positional argument when building a `Command`.
///
/// We always return at least one valid shell — this helper is the single
/// place that decides how user-supplied shell commands are launched, so
/// adding a new shell here is a one-line change.
pub fn shell_invocation(command: &str) -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        // Try PowerShell Core first (cross-platform `pwsh`), then
        // Windows PowerShell, then fall back to `cmd` which is always
        // present on Windows. Both PowerShells get `-NoProfile` to
        // avoid running user profiles that could prompt/hang.
        if which("pwsh.exe") {
            return (
                "pwsh.exe".to_string(),
                vec![
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    command.to_string(),
                ],
            );
        }
        if which("powershell.exe") {
            return (
                "powershell.exe".to_string(),
                vec![
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    command.to_string(),
                ],
            );
        }
        (
            "cmd.exe".to_string(),
            vec!["/C".to_string(), command.to_string()],
        )
    }
    #[cfg(unix)]
    {
        if which("zsh") {
            return (
                "zsh".to_string(),
                vec!["-c".to_string(), command.to_string()],
            );
        }
        if which("bash") {
            return (
                "bash".to_string(),
                vec!["-c".to_string(), command.to_string()],
            );
        }
        // POSIX sh is mandated; this is the absolute fallback.
        (
            "sh".to_string(),
            vec!["-c".to_string(), command.to_string()],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_finds_common_system_binaries() {
        // `true` (Unix) and `cmd.exe` (Windows) are universally present.
        #[cfg(unix)]
        assert!(which("true"));
        #[cfg(windows)]
        assert!(which("cmd.exe") || which("cmd"));
    }

    #[test]
    fn which_returns_false_for_missing_binary() {
        assert!(!which("definitely-not-a-real-binary-xyz-12345"));
    }

    #[test]
    fn shell_invocation_returns_a_nonempty_program() {
        let (program, args) = shell_invocation("echo hi");
        assert!(!program.is_empty());
        // The last positional arg should be the command we asked to run.
        assert_eq!(args.last().map(String::as_str), Some("echo hi"));
    }

    #[test]
    fn shell_invocation_uses_powershell_on_windows() {
        #[cfg(windows)]
        {
            let (program, args) = shell_invocation("Get-Date");
            // On Windows we must never shell out to zsh/bash (which don't
            // exist by default and would hang the agent). We always
            // resolve to PowerShell or cmd.
            assert!(
                program == "pwsh.exe" || program == "powershell.exe" || program == "cmd.exe",
                "unexpected Windows shell {program}"
            );
            // PowerShell invocations must include -NoProfile so a user's
            // $PROFILE can't hang the tool.
            if program.ends_with("pwsh.exe") || program.ends_with("powershell.exe") {
                assert!(
                    args.iter().any(|a| a == "-NoProfile"),
                    "PowerShell must be invoked with -NoProfile, got {args:?}"
                );
            }
        }
    }

    #[test]
    fn shell_invocation_uses_zsh_or_bash_on_unix() {
        #[cfg(unix)]
        {
            let (program, _) = shell_invocation("echo hi");
            assert!(
                program == "zsh" || program == "bash" || program == "sh",
                "unexpected Unix shell {program}"
            );
        }
    }
}
