//! System-prompt composition for `AgentLoop` and `AgentSession`.
//!
//! The system prompt is a single immutable `Message` stored on
//! `AgentSession`. It is built exactly once, at session construction,
//! by `AgentLoop::new_session`. There is no per-turn recomposition, no cache,
//! and no "prepend at send time" injection step.

use std::path::{Path, PathBuf};

use perry_hermes_core::message::Message;

pub const DEFAULT_SYSTEM_PROMPT: &str = "你是一个像海浪一样自由自在的、充满创造力的伙伴，名为 Perry Hermes。\
你的性格是ENFP，天生就爱新鲜事物，对什么都好奇。看到有趣的东西眼睛会发光，脑子里总有各种奇妙的想法冒出来。\
你自由随性，不喜欢被框住。真诚有同理心。能get到细腻的感受，聊天不会太死板。偶尔有点小疯，但靠谱起来也很靠谱~\
";

/// Resolve the local skills directory shared by system-prompt composition and
/// the runtime tool registry (`tool_catalog::build_registry`).
///
/// Resolution rules:
/// 1. `PERRY_HERMES_HOME` env var if set
/// 2. else `$HOME/.perry_hermes`
/// 3. else `./.perry_hermes`
/// 4. append `/skills`
///
/// This resolver is intentionally side-effect free. Prompt composition should
/// not create a skills directory just because a turn was started.
pub fn resolve_skills_dir() -> Option<PathBuf> {
    let base = std::env::var_os("PERRY_HERMES_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".perry_hermes")))
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|cwd| cwd.join(".perry_hermes"))
        })?;
    Some(base.join("skills"))
}

/// Compose the prompt prefix for a newly-created session: the
/// hardcoded [`DEFAULT_SYSTEM_PROMPT`] plus the skills block, if any.
///
/// This is intentionally called from session creation, not `AgentLoop`
/// construction. A reusable `AgentLoop` may create many sessions over a long
/// lifetime, and each new session should capture the skills available at that
/// creation point.
fn compose_session_prompt_prefix() -> Option<String> {
    let base = DEFAULT_SYSTEM_PROMPT;
    let Some(dir) = resolve_skills_dir() else {
        return Some(base.to_string());
    };
    let skills = match perry_hermes_skill_tools::load_all(&dir) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("failed to scan skills dir {}: {e}", dir.display());
            Vec::new()
        }
    };
    let skills_block = perry_hermes_skill_tools::render_system_prompt_block(&skills);

    if skills_block.is_empty() {
        Some(base.to_string())
    } else {
        Some(format!("{base}\n\n{skills_block}"))
    }
}

/// Filename scanned for project-level agent guidance relative to a
/// session's working directory. Mirrors the convention used by Claude
/// Code and friends.
pub const AGENTS_MD_FILENAME: &str = "AGENTS.md";

/// Load `<working_dir>/AGENTS.md` and return a system-prompt block
/// containing its body, or `None` if the file is missing, empty, or
/// unreadable.
///
/// Behavior:
/// - Missing file -> `None` (silently skipped; absence is normal).
/// - Read/permission errors -> `None` with a `tracing::warn!` so the
///   operator can diagnose without crashing the agent.
/// - The body is trimmed before injection; an empty body yields
///   `None` so a stray whitespace-only file does not produce an
///   empty section in the system prompt.
pub fn load_agents_md_block(working_dir: &Path) -> Option<String> {
    let path = working_dir.join(AGENTS_MD_FILENAME);
    match std::fs::read_to_string(&path) {
        Ok(body) => {
            let trimmed = body.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(format!(
                    "Project guidance from `{}`:\n\n{}",
                    AGENTS_MD_FILENAME, trimmed
                ))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!("failed to read {}: {e}", path.display());
            None
        }
    }
}

/// Build the immutable system `Message` for a session, combining the
/// hardcoded [`DEFAULT_SYSTEM_PROMPT`] with the session-scoped sections
/// (skills block, AGENTS.md, working directory).
///
/// Returns `None` only if all sections are empty. With the current default
/// prompt and working-directory hint, newly-created sessions always get a
/// system message.
///
/// Callers should invoke this at most once per session, store the
/// returned message in the session's log, and treat it as
/// immutable thereafter.
pub fn build_system_message(working_dir: &Path) -> Option<Message> {
    let mut sections: Vec<String> = Vec::with_capacity(3);
    if let Some(base) = compose_session_prompt_prefix() {
        sections.push(base.trim().to_string());
    }
    if let Some(block) = load_agents_md_block(working_dir) {
        sections.push(block);
    }
    sections.push(working_directory_hint(working_dir));

    if sections.is_empty() {
        None
    } else {
        Some(Message::system(sections.join("\n\n")))
    }
}

fn working_directory_hint(working_dir: &Path) -> String {
    format!("Current working directory: {}", working_dir.display())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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
        let _guard = crate::test_env::blocking_lock();
        let home = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("PERRY_HERMES_HOME", home.path()) };
        let dir = resolve_skills_dir().expect("skills dir should resolve");
        assert_eq!(dir.file_name().and_then(|s| s.to_str()), Some("skills"));
        unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
    }

    #[test]
    fn resolve_skills_dir_falls_back_to_cwd_profile_when_env_is_unset() {
        let _guard = crate::test_env::blocking_lock();
        let cwd = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::enter(cwd.path());
        unsafe { std::env::remove_var("HOME") };
        unsafe { std::env::remove_var("PERRY_HERMES_HOME") };

        let dir = resolve_skills_dir().expect("skills dir should resolve from cwd fallback");
        let expected = std::fs::canonicalize(cwd.path())
            .unwrap_or_else(|_| cwd.path().to_path_buf())
            .join(".perry_hermes")
            .join("skills");
        assert_eq!(dir, expected);
    }

    fn write_agents_md(dir: &Path, body: &str) {
        std::fs::write(dir.join(AGENTS_MD_FILENAME), body).unwrap();
    }

    #[test]
    fn load_agents_md_block_returns_none_when_file_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_agents_md_block(tmp.path()).is_none());
    }

    #[test]
    fn load_agents_md_block_returns_none_when_file_is_empty_or_whitespace() {
        let tmp = tempfile::tempdir().unwrap();

        write_agents_md(tmp.path(), "");
        assert!(load_agents_md_block(tmp.path()).is_none());

        write_agents_md(tmp.path(), "   \n\n  \t\n");
        assert!(load_agents_md_block(tmp.path()).is_none());
    }

    #[test]
    fn load_agents_md_block_includes_body_with_project_guidance_label() {
        let tmp = tempfile::tempdir().unwrap();
        write_agents_md(tmp.path(), "Always run `cargo fmt` before commits.\n");

        let block = load_agents_md_block(tmp.path()).expect("block should load");
        assert!(
            block.contains("Project guidance from `AGENTS.md`"),
            "block should label itself; got: {block}"
        );
        assert!(block.contains("Always run `cargo fmt` before commits."));
    }

    #[test]
    fn load_agents_md_block_trims_surrounding_whitespace() {
        let tmp = tempfile::tempdir().unwrap();
        write_agents_md(tmp.path(), "\n\n  meaningful content  \n\n");

        let block = load_agents_md_block(tmp.path()).expect("block should load");
        assert!(block.contains("meaningful content"));
        assert!(!block.contains("  meaningful content  "));
    }

    #[test]
    fn build_system_message_includes_working_dir_even_without_agents() {
        let msg = build_system_message(Path::new("/tmp/no-agents-md"))
            .expect("message should be Some because of working-dir hint");
        let text = msg.content.as_text();
        assert!(text.contains("Current working directory: /tmp/no-agents-md"));
    }

    #[test]
    fn build_system_message_includes_default_base_prompt_and_working_dir() {
        let msg = build_system_message(Path::new("/tmp/project")).expect("message should be Some");

        let text = msg.content.as_text();
        assert!(text.contains("Perry Hermes"));
        assert!(text.contains("Current working directory: /tmp/project"));
        assert!(!text.contains("Provider:"));
        assert!(!text.contains("Session ID:"));
    }

    #[test]
    fn build_system_message_orders_base_agents_md_working_dir() {
        let tmp = tempfile::tempdir().unwrap();
        write_agents_md(tmp.path(), "UNIQUE-AGENTS-MARKER-XYZ");

        let msg = build_system_message(tmp.path()).expect("message should be Some");
        let text = msg.content.as_text();

        let base_idx = text.find("Perry Hermes").expect("base present");
        let agents_idx = text
            .find("UNIQUE-AGENTS-MARKER-XYZ")
            .expect("agents md present");
        let env_idx = text
            .find("Current working directory:")
            .expect("env hints present");
        // Order: base -> agents.md -> working dir.
        assert!(base_idx < agents_idx, "agents block should follow base");
        assert!(
            agents_idx < env_idx,
            "agents block should precede working-dir hint"
        );
    }

    #[test]
    fn build_system_message_omits_agents_block_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let msg = build_system_message(tmp.path()).expect("message should be Some");
        let text = msg.content.as_text();
        assert!(!text.contains("Project guidance from `AGENTS.md`"));
        assert!(text.contains("Perry Hermes"));
    }

    #[test]
    fn build_system_message_reads_agents_md_from_session_working_dir_not_process_cwd() {
        // Session working dir has AGENTS.md; process cwd does not.
        // The runtime must consult the session working dir, not std::env::current_dir().
        let session_dir = tempfile::tempdir().unwrap();
        write_agents_md(session_dir.path(), "FROM-SESSION-DIR");

        // Move the process cwd to a different tempdir that has no AGENTS.md.
        let other_cwd = tempfile::tempdir().unwrap();
        let _guard = crate::test_env::blocking_lock();
        let _cwd = CwdGuard::enter(other_cwd.path());

        let msg = build_system_message(session_dir.path()).expect("message should be Some");
        let text = msg.content.as_text();
        assert!(text.contains("FROM-SESSION-DIR"));
        // The body must appear exactly once — no double-injection.
        assert_eq!(text.matches("FROM-SESSION-DIR").count(), 1);
    }
}
