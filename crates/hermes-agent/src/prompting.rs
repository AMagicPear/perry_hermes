use std::path::PathBuf;

use chrono::Local;
use chrono_tz::Tz;
use hermes_core::message::{Message, Role};

use crate::session::SessionContext;

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
/// 3. else `./.perry_hermes`
/// 4. append `/skills`
///
/// This resolver is intentionally side-effect free. Prompt composition should
/// not create a skills directory just because a turn was started.
pub fn resolve_skills_dir() -> Option<PathBuf> {
    let base = std::env::var_os("HERMES_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".perry_hermes")))
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|cwd| cwd.join(".perry_hermes"))
        })?;
    Some(base.join("skills"))
}

pub fn compose_base_system_prompt(user_prompt: Option<&str>) -> Option<String> {
    let base = user_prompt.unwrap_or(DEFAULT_SYSTEM_PROMPT);
    let Some(dir) = resolve_skills_dir() else {
        return Some(base.to_string());
    };
    let skills = match hermes_skill_loader::load_all(&dir) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("failed to scan skills dir {}: {e}", dir.display());
            Vec::new()
        }
    };
    let skills_block = hermes_skill_loader::render_system_prompt_block(&skills);

    if skills_block.is_empty() {
        Some(base.to_string())
    } else {
        Some(format!("{base}\n\n{skills_block}"))
    }
}

pub fn build_runtime_system_prompt(
    base_prompt: &str,
    session: &SessionContext,
    provider_name: Option<&str>,
) -> String {
    let mut sections = vec![base_prompt.trim().to_string()];

    let env_hints = build_environment_hints(session);
    if !env_hints.is_empty() {
        sections.push(env_hints);
    }

    let metadata = build_conversation_metadata(session, provider_name);
    if !metadata.is_empty() {
        sections.push(metadata);
    }

    sections.join("\n\n")
}

pub fn inject_system_prompt(messages: Vec<Message>, system_prompt: Option<String>) -> Vec<Message> {
    let Some(system_prompt) = system_prompt else {
        return messages;
    };
    if messages.iter().any(|m| m.role == Role::System) {
        return messages;
    }
    let mut with_system = Vec::with_capacity(messages.len() + 1);
    with_system.push(Message::system(system_prompt));
    with_system.extend(messages);
    with_system
}

fn build_environment_hints(session: &SessionContext) -> String {
    let mut lines = Vec::new();
    let host = if cfg!(target_os = "macos") {
        "Host: macOS".to_string()
    } else if cfg!(target_os = "windows") {
        "Host: Windows".to_string()
    } else {
        format!("Host: {}", std::env::consts::OS)
    };
    lines.push(host);

    if let Some(home) = std::env::var_os("HOME") {
        lines.push(format!(
            "User home directory: {}",
            PathBuf::from(home).display()
        ));
    }
    lines.push(format!(
        "Current working directory: {}",
        session.working_dir.display()
    ));
    lines.join("\n")
}

fn build_conversation_metadata(session: &SessionContext, provider_name: Option<&str>) -> String {
    let now = hermes_now();
    let mut lines = vec![format!(
        "Conversation started: {}",
        now.format("%A, %B %d, %Y")
    )];
    if !session.session_id.is_empty() {
        lines.push(format!("Session ID: {}", session.session_id));
    }
    if let Some(provider) = provider_name {
        lines.push(format!("Provider: {provider}"));
    }
    lines.join("\n")
}

fn hermes_now() -> chrono::DateTime<chrono::FixedOffset> {
    if let Some(name) = std::env::var("HERMES_TIMEZONE")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        if let Ok(tz) = name.parse::<Tz>() {
            return chrono::Utc::now().with_timezone(&tz).fixed_offset();
        }
    }
    Local::now().fixed_offset()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::path::Path;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct CwdGuard {
        previous: PathBuf,
    }

    impl CwdGuard {
        fn enter(dir: &Path) -> Self {
            let previous = std::env::current_dir().unwrap();
            std::env::set_current_dir(dir).unwrap();
            Self { previous }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous);
        }
    }

    #[test]
    fn resolve_returns_a_directory_path_that_ends_in_skills() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("HERMES_HOME", home.path()) };
        let dir = resolve_skills_dir().expect("skills dir should resolve");
        assert_eq!(dir.file_name().and_then(|s| s.to_str()), Some("skills"));
        unsafe { std::env::remove_var("HERMES_HOME") };
    }

    #[test]
    fn resolve_skills_dir_falls_back_to_cwd_profile_when_env_is_unset() {
        let _guard = ENV_LOCK.lock().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::enter(cwd.path());
        unsafe { std::env::remove_var("HOME") };
        unsafe { std::env::remove_var("HERMES_HOME") };

        let dir = resolve_skills_dir().expect("skills dir should resolve from cwd fallback");
        let expected = std::fs::canonicalize(cwd.path())
            .unwrap_or_else(|_| cwd.path().to_path_buf())
            .join(".perry_hermes")
            .join("skills");
        assert_eq!(dir, expected);
    }

    #[test]
    fn build_runtime_system_prompt_includes_session_metadata() {
        unsafe { std::env::set_var("HERMES_TIMEZONE", "UTC") };
        unsafe { std::env::set_var("HOME", "/tmp/perry-home") };

        let prompt = build_runtime_system_prompt(
            "BASE",
            &SessionContext {
                working_dir: PathBuf::from("/tmp/project"),
                session_id: "session-123".into(),
            },
            Some("echo"),
        );

        assert!(prompt.contains("BASE"));
        assert!(prompt.contains("Current working directory: /tmp/project"));
        assert!(prompt.contains("User home directory: /tmp/perry-home"));
        assert!(prompt.contains("Session ID: session-123"));
        assert!(prompt.contains("Provider: echo"));
        assert!(prompt.contains(&format!(
            "Conversation started: {}",
            Utc::now().format("%A, %B %d, %Y")
        )));
    }

    #[test]
    fn inject_system_prompt_is_noop_when_messages_already_have_system_role() {
        let messages = vec![Message::system("existing")];

        let injected = inject_system_prompt(messages.clone(), Some("new prompt".into()));
        assert_eq!(injected.len(), 1);
        assert_eq!(injected[0].content.as_text(), "existing");
    }
}
