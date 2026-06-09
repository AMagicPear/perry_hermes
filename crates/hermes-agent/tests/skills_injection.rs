//! End-to-end tests for the skills injection wiring in `AIAgent::from_config`.
//!
//! These tests use a scripted provider that captures the `messages`
//! passed to its `stream` call. The captured system message is asserted
//! to contain (or not contain) the expected skills block.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use perry_hermes_agent::{AIAgent, PerryHermesConfig};
use perry_hermes_core::message::Message;
use perry_hermes_core::provider::{CompletionDelta, CompletionStream, FinishReason, Provider};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

/// Serialize tests that mutate process-wide state (HOME/PERRY_HERMES_HOME).
///
///
/// The runtime reads those vars during `run_turn`, so the guard must stay
/// alive across the entire async turn, not just the `set_var` calls.
async fn with_env_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static ENV_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());
    ENV_LOCK.lock().await
}

#[derive(Default)]
struct CaptureProvider {
    captured: Arc<Mutex<Vec<Message>>>,
}

#[async_trait]
impl Provider for CaptureProvider {
    async fn stream(
        &self,
        messages: &[Message],
        _tools: &[perry_hermes_core::registry::ToolSchema],
        _cancel: CancellationToken,
    ) -> Result<CompletionStream, perry_hermes_core::ProviderError> {
        *self.captured.lock().unwrap() = messages.to_vec();
        Ok(Box::pin(stream::iter(vec![Ok(CompletionDelta {
            content_delta: Some("ok".into()),
            reasoning_delta: None,
            tool_call_delta: None,
            usage: Some(perry_hermes_core::Usage::default()),
            finish_reason: Some(FinishReason::Stop),
        })])))
    }
}

fn write_skill(root: &std::path::Path, rel: &str, contents: &str) {
    use std::fs;
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&p, contents).unwrap();
}

fn skills_dir_for(home: &std::path::Path) -> PathBuf {
    home.join(".perry_hermes").join("skills")
}

fn config_for_echo() -> PerryHermesConfig {
    perry_hermes_agent::test_helpers::PerryHermesConfig::for_test_echo()
}

fn system_text(messages: &[Message]) -> String {
    let system = messages
        .iter()
        .find(|m| m.role == perry_hermes_core::message::Role::System)
        .expect("a System message should have been injected");
    match &system.content {
        perry_hermes_core::message::Content::Text(s) => s.clone(),
        _ => panic!("system message should be text"),
    }
}

#[tokio::test]
async fn runtime_new_preserves_user_prompt_without_skills_dir() {
    let _guard = with_env_lock().await;
    unsafe { std::env::remove_var("HOME") };
    unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
    unsafe { std::env::set_var("PERRY_HERMES_TIMEZONE", "UTC") };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let mut config = config_for_echo();
    config.agent.system_prompt = Some("ONLY-CUSTOM".into());
    let agent = AIAgent::new(provider, config);
    let session = agent.new_session("t", PathBuf::from("/tmp"));

    agent
        .run_session_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let text = system_text(&msgs);
    assert!(text.contains("ONLY-CUSTOM"));
    assert!(text.contains("Current working directory: /tmp"));
}

#[tokio::test]
async fn runtime_uses_default_system_prompt_when_config_omits_it_and_skills_dir_absent() {
    let _guard = with_env_lock().await;
    unsafe { std::env::set_var("HOME", "/definitely/does/not/exist/perry-hermes-test") };
    unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
    unsafe { std::env::set_var("PERRY_HERMES_TIMEZONE", "UTC") };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let agent = AIAgent::new(provider, config_for_echo());
    let session = agent.new_session("t", PathBuf::from("/tmp"));
    agent
        .run_session_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let text = system_text(&msgs);
    assert!(text.contains("careful assistant"));
    assert!(text.contains("Current working directory: /tmp"));
    assert!(!text.contains("Available skills"));
}

#[tokio::test]
async fn runtime_appends_skills_block_after_user_supplied_system_prompt() {
    // tempdir must live for the entire test (set HOME inside the lock, but
    // don't let tempdir get dropped when the lock-scope ends).
    let home = tempfile::tempdir().unwrap();
    let skills = skills_dir_for(home.path());
    write_skill(
        &skills,
        "rust-core-style/SKILL.md",
        "---\nname: rust-core-style\ndescription: \"Rust style\"\n---\nbody\n",
    );
    let _guard = with_env_lock().await;
    unsafe { std::env::set_var("HOME", home.path()) };
    unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
    unsafe { std::env::set_var("PERRY_HERMES_TIMEZONE", "UTC") };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let mut config = config_for_echo();
    config.agent.system_prompt = Some("CUSTOM-PROMPT-MARKER".into());
    let agent = AIAgent::new(provider, config);

    let session = agent.new_session("t", PathBuf::from("/tmp"));
    agent
        .run_session_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let text = system_text(&msgs);
    let custom_idx = text
        .find("CUSTOM-PROMPT-MARKER")
        .expect("custom prompt present");
    let skills_idx = text.find("Available skills").expect("skills block present");
    assert!(custom_idx < skills_idx);
    assert!(text.contains("**rust-core-style**: Rust style"));
    assert!(text.contains("Current working directory: /tmp"));
}

#[tokio::test]
async fn runtime_reads_skills_when_session_is_created_not_when_agent_is_created() {
    let home = tempfile::tempdir().unwrap();
    let _guard = with_env_lock().await;
    unsafe { std::env::set_var("HOME", home.path()) };
    unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
    unsafe { std::env::set_var("PERRY_HERMES_TIMEZONE", "UTC") };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let mut config = config_for_echo();
    config.agent.system_prompt = Some("CUSTOM-PROMPT-MARKER".into());
    let agent = AIAgent::new(provider, config);

    write_skill(
        &skills_dir_for(home.path()),
        "created-after-agent/SKILL.md",
        "---\nname: created-after-agent\ndescription: \"created later\"\n---\nbody\n",
    );

    let session = agent.new_session("t", PathBuf::from("/tmp"));
    agent
        .run_session_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let text = system_text(&msgs);
    assert!(text.contains("CUSTOM-PROMPT-MARKER"));
    assert!(text.contains("**created-after-agent**: created later"));
}

#[tokio::test]
async fn runtime_does_not_fail_construction_when_skills_dir_has_parse_errors() {
    let home = tempfile::tempdir().unwrap();
    let skills = skills_dir_for(home.path());
    write_skill(&skills, "bad-fm/SKILL.md", "no frontmatter at all\n");
    write_skill(
        &skills,
        "ok-skill/SKILL.md",
        "---\nname: ok-skill\ndescription: \"fine\"\n---\nbody\n",
    );
    let _guard = with_env_lock().await;
    unsafe { std::env::set_var("HOME", home.path()) };
    unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
    unsafe { std::env::set_var("PERRY_HERMES_TIMEZONE", "UTC") };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let agent = AIAgent::new(provider, config_for_echo());
    let session = agent.new_session("t", PathBuf::from("/tmp"));
    agent
        .run_session_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let text = system_text(&msgs);
    assert!(text.contains("**ok-skill**"));
    assert!(!text.contains("bad-fm"));
}

#[tokio::test]
async fn runtime_uses_default_system_prompt_when_home_is_unset() {
    let _guard = with_env_lock().await;
    unsafe { std::env::remove_var("HOME") };
    unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
    unsafe { std::env::set_var("PERRY_HERMES_TIMEZONE", "UTC") };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let agent = AIAgent::new(provider, config_for_echo());
    let session = agent.new_session("t", PathBuf::from("/tmp"));
    agent
        .run_session_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let text = system_text(&msgs);
    assert!(text.contains("careful assistant"));
    assert!(text.contains("Current working directory: /tmp"));
    assert!(!text.contains("Available skills"));
    assert!(!text.contains("skill_view"));
}

#[tokio::test]
async fn runtime_injects_skills_index_into_system_prompt_when_skills_dir_present() {
    let home = tempfile::tempdir().unwrap();
    let skills = skills_dir_for(home.path());
    write_skill(
        &skills,
        "rust-core-style/SKILL.md",
        "---\nname: rust-core-style\ndescription: \"Rust style\"\n---\n",
    );
    write_skill(
        &skills,
        "software-engineering/dogfood/SKILL.md",
        "---\nname: dogfood\ndescription: \"QA workflow\"\n---\n",
    );
    let _guard = with_env_lock().await;
    unsafe { std::env::set_var("HOME", home.path()) };
    unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
    unsafe { std::env::set_var("PERRY_HERMES_TIMEZONE", "UTC") };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let agent = AIAgent::new(provider, config_for_echo());
    let session = agent.new_session("t", PathBuf::from("/tmp"));
    agent
        .run_session_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let text = system_text(&msgs);
    assert!(text.contains("Available skills"));
    assert!(text.contains("rust-core-style"));
    assert!(text.contains("QA workflow"));
    assert!(text.contains("skill_view"));
}

#[tokio::test]
async fn runtime_includes_working_directory_in_system_message() {
    // Sanity check: the system message always reflects the session's
    // working directory. The PERRY_HERMES_TIMEZONE / HOME settings
    // are kept here only because the test suite serializes env-var
    // mutations through a shared lock; they no longer affect the
    // system prompt.
    let _guard = with_env_lock().await;
    unsafe { std::env::remove_var("HOME") };
    unsafe { std::env::remove_var("PERRY_HERMES_HOME") };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let agent = AIAgent::new(provider, config_for_echo());
    let session = agent.new_session("cwd", PathBuf::from("/tmp/cwd-check"));

    agent
        .run_session_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let text = system_text(&msgs);
    assert!(text.contains("Current working directory: /tmp/cwd-check"));
}

#[tokio::test]
async fn runtime_injects_agents_md_from_session_working_dir() {
    // Create a tempdir that doubles as the session working dir AND the
    // location of an AGENTS.md file. The runtime should read the file
    // and inject its body into the system prompt.
    let project = tempfile::tempdir().unwrap();
    let agents_md = project.path().join("AGENTS.md");
    std::fs::write(
        &agents_md,
        "Always answer in haiku.\n\n# Rules\n- be concise\n",
    )
    .unwrap();

    let _guard = with_env_lock().await;
    unsafe { std::env::set_var("HOME", "/definitely/does/not/exist/perry-hermes-test") };
    unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
    unsafe { std::env::set_var("PERRY_HERMES_TIMEZONE", "UTC") };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let agent = AIAgent::new(provider, config_for_echo());
    let session = agent.new_session("agents-md", project.path().to_path_buf());

    agent
        .run_session_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let text = system_text(&msgs);
    assert!(
        text.contains("Project guidance from `AGENTS.md`"),
        "system prompt should label the AGENTS.md block; got:\n{text}"
    );
    assert!(text.contains("Always answer in haiku."));
    assert!(text.contains("# Rules"));
    // AGENTS.md should sit between the base prompt and the env hints.
    let base_idx = text.find("careful assistant").expect("base prompt present");
    let agents_idx = text
        .find("Project guidance from `AGENTS.md`")
        .expect("agents md block present");
    let env_idx = text
        .find("Current working directory:")
        .expect("env hints present");
    assert!(base_idx < agents_idx);
    assert!(agents_idx < env_idx);
}

#[tokio::test]
async fn runtime_omits_agents_md_block_when_file_absent_in_working_dir() {
    let project = tempfile::tempdir().unwrap();
    // No AGENTS.md is written.
    let _guard = with_env_lock().await;
    unsafe { std::env::set_var("HOME", "/definitely/does/not/exist/perry-hermes-test") };
    unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
    unsafe { std::env::set_var("PERRY_HERMES_TIMEZONE", "UTC") };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let agent = AIAgent::new(provider, config_for_echo());
    let session = agent.new_session("no-agents-md", project.path().to_path_buf());

    agent
        .run_session_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let text = system_text(&msgs);
    assert!(
        !text.contains("Project guidance from `AGENTS.md`"),
        "no AGENTS.md block should be injected when the file is missing"
    );
}
