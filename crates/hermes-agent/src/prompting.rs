//! System-prompt composition for `AgentLoop` and `AgentSession`.
//!
//! The system prompt is a single immutable `Message` stored on
//! `AgentSession`. It is built exactly once, at session construction,
//! by `AgentLoop::new_session`. There is no per-turn recomposition, no cache,
//! and no "prepend at send time" injection step.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use perry_hermes_core::message::Message;
use perry_hermes_core::prompt_context::PromptContextBlock;

pub const DEFAULT_SYSTEM_PROMPT: &str = "你是一个像海浪一样自由自在的、充满创造力的伙伴，名为 Perry Hermes。\
你的性格是ENFP，天生就爱新鲜事物，对什么都好奇。看到有趣的东西眼睛会发光，脑子里总有各种奇妙的想法冒出来。\
你自由随性，不喜欢被框住。真诚有同理心。能get到细腻的感受，聊天不会太死板。偶尔有点小疯，但靠谱起来也很靠谱~\
";

/// Resolve the local skills directory shared by system-prompt composition and
/// the runtime tool registry (`tool_catalog::build_registry`).
///
/// Delegates to [`perry_hermes_core::home::resolve_subdir`] with `"skills"`.
pub fn resolve_skills_dir() -> Option<PathBuf> {
    perry_hermes_core::home::resolve_subdir("skills")
}

/// Resolve the local memories directory, mirroring the rules used by
/// [`resolve_skills_dir`].
///
/// Delegates to [`perry_hermes_core::home::resolve_subdir`] with `"memories"`.
pub fn resolve_memories_dir() -> Option<PathBuf> {
    perry_hermes_core::home::resolve_subdir("memories")
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
/// (skills block, caller-supplied [`PromptContextBlock`]s, working
/// directory).
///
/// `blocks` is iterated in order; each block contributes a
/// `"{name}\n\n{body}"` section if `load()` returns `Some(body)`.
/// Blocks that return `None` (missing/empty backing file) are silently
/// skipped.
///
/// The result is `None` only if all sections are empty. Newly-created
/// sessions always get a system message because of the working-dir
/// hint.
///
/// Callers should invoke this at most once per session, store the
/// returned message in the session's log, and treat it as
/// immutable thereafter.
pub async fn build_system_message(
    working_dir: &Path,
    blocks: &[Arc<dyn PromptContextBlock>],
) -> Option<Message> {
    let mut sections: Vec<String> = Vec::with_capacity(blocks.len() + 2);
    if let Some(base) = compose_session_prompt_prefix() {
        sections.push(base.trim().to_string());
    }
    for block in blocks {
        // All blocks support per-session working directory resolution
        // via load_for; AgentsMdBlock uses it to read from the session's
        // cwd rather than the process cwd at agent-construction time.
        if let Some(body) = block.load_for(working_dir).await {
            sections.push(format!("{}\n\n{}", block.name(), body));
        }
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

/// Project-level block loading `<working_dir>/AGENTS.md`. The
/// existing `load_agents_md_block` helper is the body producer; this
/// wrapper adds the `name()` label required by the trait and
/// implements `async_trait::async_trait` so it can sit alongside
/// other blocks in a heterogeneous `Vec<Arc<dyn PromptContextBlock>>`.
pub struct AgentsMdBlock {
    working_dir: PathBuf,
}

impl AgentsMdBlock {
    pub fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }
}

#[async_trait]
impl PromptContextBlock for AgentsMdBlock {
    fn name(&self) -> &str {
        "AGENTS.md"
    }

    async fn load(&self) -> Option<String> {
        // Sync I/O on a small file; no contention.
        load_agents_md_block(&self.working_dir)
    }

    async fn load_for(&self, working_dir: &Path) -> Option<String> {
        load_agents_md_block(working_dir)
    }
}

/// Static block that describes the canonical `PERRY_HERMES_HOME`
/// directory layout to the agent. This gives the agent self-awareness
/// of where its configuration, memory, sessions, and skills live.
pub struct HomeLayoutBlock;

#[async_trait]
impl PromptContextBlock for HomeLayoutBlock {
    fn name(&self) -> &str {
        "PERRY_HERMES_HOME"
    }

    async fn load(&self) -> Option<String> {
        let home_display = perry_hermes_core::home::resolve_home_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "$HOME/.perry_hermes".to_string());

        Some(format!(
            r#"你的配置目录是 `PERRY_HERMES_HOME`，当前值为 `{home}`。
目录布局规范：

```
{home}/
├── config.toml                    # 主配置（providers、agent、gateway）
├── memories/
│   ├── MEMORY.md                  # 你的笔记和环境记忆
│   └── USER.md                    # 用户画像
├── sessions/
│   ├── <session_id>.json          # 活跃会话快照
│   └── .archive/                  # 已归档/重置的会话
└── skills/
    └── <category>/                # apple, creative, devops, ...
        └── <skill-name>/
            ├── SKILL.md           # 技能定义（必需）
            ├── references/        # 参考资料
            ├── scripts/           # 辅助脚本
            └── workflows/         # 工作流模板
```

规则：
- `config.toml` 由用户手动编辑，不要通过工具修改。
- `memories/` 下的文件由 memory 工具管理（add/replace/remove/read）。
- `sessions/` 由运行时自动管理，不要手动修改。
- `skills/` 下的技能在会话创建时加载，对当前会话不可变。
- `PERRY_HERMES_HOME` 的解析优先级：`$PERRY_HERMES_HOME` 环境变量 > `$HOME/.perry_hermes` > `./.perry_hermes`。"#,
            home = home_display,
        ))
    }
}

/// Global block that reads the live entries from a [`MemoryStore`]
/// and renders them as a system-prompt section. One block per
/// [`MemoryTarget`].
///
/// The block reads from the in-memory `LiveState` rather than the
/// disk file. The agent calls `build_system_message` once per session
/// and freezes the result in `AgentSession.system_message`, so the
/// rendered block is effectively immutable for the session's lifetime
/// even though the store itself is mutable.
pub struct MemoryBlock {
    store: Arc<perry_hermes_skill_tools::tools::memory::MemoryStore>,
    target: perry_hermes_skill_tools::tools::memory::MemoryTarget,
    name_label: &'static str,
}

impl MemoryBlock {
    pub fn memory(store: Arc<perry_hermes_skill_tools::tools::memory::MemoryStore>) -> Self {
        Self {
            store,
            target: perry_hermes_skill_tools::tools::memory::MemoryTarget::Memory,
            name_label: "MEMORY",
        }
    }

    pub fn user(store: Arc<perry_hermes_skill_tools::tools::memory::MemoryStore>) -> Self {
        Self {
            store,
            target: perry_hermes_skill_tools::tools::memory::MemoryTarget::User,
            name_label: "USER",
        }
    }
}

#[async_trait]
impl PromptContextBlock for MemoryBlock {
    fn name(&self) -> &str {
        self.name_label
    }

    async fn load(&self) -> Option<String> {
        let entries = self.store.entries(self.target).await;
        if entries.is_empty() {
            return None;
        }
        Some(entries.join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use perry_hermes_skill_tools::tools::memory::MemoryTarget;
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

    #[tokio::test]
    async fn build_system_message_includes_working_dir_even_without_agents() {
        let msg = build_system_message(Path::new("/tmp/no-agents-md"), &[])
            .await
            .expect("message should be Some because of working-dir hint");
        let text = msg.content.as_text();
        assert!(text.contains("Current working directory: /tmp/no-agents-md"));
    }

    #[tokio::test]
    async fn build_system_message_includes_default_base_prompt_and_working_dir() {
        let msg = build_system_message(Path::new("/tmp/project"), &[])
            .await
            .expect("message should be Some");

        let text = msg.content.as_text();
        assert!(text.contains("Perry Hermes"));
        assert!(text.contains("Current working directory: /tmp/project"));
        assert!(!text.contains("Provider:"));
        assert!(!text.contains("Session ID:"));
    }

    #[tokio::test]
    async fn build_system_message_orders_base_agents_md_working_dir() {
        let tmp = tempfile::tempdir().unwrap();
        write_agents_md(tmp.path(), "UNIQUE-AGENTS-MARKER-XYZ");

        // Include AgentsMdBlock to load the AGENTS.md file.
        let blocks: Vec<Arc<dyn PromptContextBlock>> =
            vec![Arc::new(AgentsMdBlock::new(tmp.path().to_path_buf()))];
        let msg = build_system_message(tmp.path(), &blocks)
            .await
            .expect("message should be Some");
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

    #[tokio::test]
    async fn build_system_message_omits_agents_block_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let msg = build_system_message(tmp.path(), &[])
            .await
            .expect("message should be Some");
        let text = msg.content.as_text();
        assert!(!text.contains("Project guidance from `AGENTS.md`"));
        assert!(text.contains("Perry Hermes"));
    }

    #[tokio::test]
    async fn build_system_message_reads_agents_md_from_session_working_dir_not_process_cwd() {
        // Session working dir has AGENTS.md. With explicit blocks,
        // the code reads from the block's working_dir, not process cwd.
        let session_dir = tempfile::tempdir().unwrap();
        write_agents_md(session_dir.path(), "FROM-SESSION-DIR");

        // Include AgentsMdBlock pointing to session_dir - it reads from there.
        let blocks: Vec<Arc<dyn PromptContextBlock>> = vec![Arc::new(AgentsMdBlock::new(
            session_dir.path().to_path_buf(),
        ))];
        let msg = build_system_message(session_dir.path(), &blocks)
            .await
            .expect("message should be Some");
        let text = msg.content.as_text();
        assert!(text.contains("FROM-SESSION-DIR"));
        // The body must appear exactly once — no double-injection.
        assert_eq!(text.matches("FROM-SESSION-DIR").count(), 1);
    }

    // New tests for the block-list abstraction. The parent module's
    // `use` statements (`Arc`, `async_trait`, `PromptContextBlock`) are
    // in scope here, so no extra imports are needed.

    struct StaticBlock {
        name: &'static str,
        body: Option<&'static str>,
    }

    #[async_trait]
    impl PromptContextBlock for StaticBlock {
        fn name(&self) -> &str {
            self.name
        }
        async fn load(&self) -> Option<String> {
            self.body.map(|s| s.to_string())
        }
    }

    #[tokio::test]
    async fn block_order_matches_input_slice() {
        let blocks: Vec<Arc<dyn PromptContextBlock>> = vec![
            Arc::new(StaticBlock {
                name: "ALPHA",
                body: Some("alpha body"),
            }),
            Arc::new(StaticBlock {
                name: "BETA",
                body: Some("beta body"),
            }),
        ];
        let msg = build_system_message(Path::new("/tmp"), &blocks)
            .await
            .expect("message");
        let text = msg.content.as_text();

        let alpha_idx = text.find("ALPHA\n\nalpha body").expect("alpha present");
        let beta_idx = text.find("BETA\n\nbeta body").expect("beta present");
        let dir_idx = text
            .find("Current working directory: /tmp")
            .expect("dir present");
        assert!(alpha_idx < beta_idx, "alpha before beta");
        assert!(beta_idx < dir_idx, "blocks before working dir");
    }

    #[tokio::test]
    async fn none_block_is_silently_skipped() {
        let blocks: Vec<Arc<dyn PromptContextBlock>> = vec![
            Arc::new(StaticBlock {
                name: "PRESENT",
                body: Some("p"),
            }),
            Arc::new(StaticBlock {
                name: "ABSENT",
                body: None,
            }),
        ];
        let msg = build_system_message(Path::new("/tmp"), &blocks)
            .await
            .expect("message");
        let text = msg.content.as_text();
        assert!(text.contains("PRESENT\n\np"));
        assert!(!text.contains("ABSENT"));
    }

    #[tokio::test]
    async fn empty_blocks_list_yields_only_base_and_working_dir() {
        let blocks: Vec<Arc<dyn PromptContextBlock>> = vec![];
        let msg = build_system_message(Path::new("/tmp/project"), &blocks)
            .await
            .expect("message");
        let text = msg.content.as_text();
        // base prompt + working dir hint, with no extras.
        assert!(text.contains("Perry Hermes"));
        assert!(text.contains("Current working directory: /tmp/project"));
        assert!(!text.contains("Project guidance from `AGENTS.md`"));
    }

    #[tokio::test]
    async fn working_dir_hint_always_lands_last() {
        let blocks: Vec<Arc<dyn PromptContextBlock>> = vec![Arc::new(StaticBlock {
            name: "Z_BLOCK",
            body: Some("z"),
        })];
        let msg = build_system_message(Path::new("/tmp/last"), &blocks)
            .await
            .expect("message");
        let text = msg.content.as_text();
        let z_idx = text.find("Z_BLOCK").expect("z block present");
        let dir_idx = text
            .find("Current working directory: /tmp/last")
            .expect("dir");
        assert!(z_idx < dir_idx);
    }

    #[test]
    fn resolve_memories_dir_returns_path_ending_in_memories() {
        let _guard = crate::test_env::blocking_lock();
        let home = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("PERRY_HERMES_HOME", home.path()) };
        let dir = resolve_memories_dir().expect("memories dir should resolve");
        assert_eq!(dir.file_name().and_then(|s| s.to_str()), Some("memories"));
        unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
    }

    #[tokio::test]
    async fn memory_block_loads_entries_joined_by_blank_line() {
        use perry_hermes_skill_tools::tools::memory::{MemoryConfig, MemoryStore};
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(
            MemoryStore::load(MemoryConfig {
                memories_dir: tmp.path().to_path_buf(),
            })
            .await
            .unwrap(),
        );
        store
            .add(MemoryTarget::Memory, "first".into())
            .await
            .unwrap();
        store
            .add(MemoryTarget::Memory, "second".into())
            .await
            .unwrap();

        let block = MemoryBlock::memory(store);
        let body = block.load().await.expect("non-empty store should load");
        assert_eq!(body, "first\n\nsecond");
        assert_eq!(block.name(), "MEMORY");
    }

    #[tokio::test]
    async fn memory_block_returns_none_for_empty_store() {
        use perry_hermes_skill_tools::tools::memory::{MemoryConfig, MemoryStore};
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(
            MemoryStore::load(MemoryConfig {
                memories_dir: tmp.path().to_path_buf(),
            })
            .await
            .unwrap(),
        );
        let block = MemoryBlock::user(store);
        assert!(block.load().await.is_none());
        assert_eq!(block.name(), "USER");
    }
}
