//! End-to-end tests for the skills injection wiring in `AIAgent::from_config`.
//!
//! These tests use a scripted provider that captures the `messages`
//! passed to its `stream` call. The captured system message is asserted
//! to contain (or not contain) the expected skills block.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use hermes_core::message::Message;
use hermes_core::provider::{
    CompletionDelta, CompletionStream, FinishReason, Provider,
};
use hermes_runtime::{AIAgent, HermesConfig, ProviderConfig, ProviderKind, SessionContext};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct CaptureProvider {
    captured: Arc<Mutex<Vec<Message>>>,
}

#[async_trait]
impl Provider for CaptureProvider {
    async fn stream(
        &self,
        messages: &[Message],
        _tools: &[hermes_core::registry::ToolSchema],
        _cancel: CancellationToken,
    ) -> Result<CompletionStream, hermes_core::ProviderError> {
        *self.captured.lock().unwrap() = messages.to_vec();
        Ok(Box::pin(stream::iter(vec![Ok(CompletionDelta {
            content_delta: Some("ok".into()),
            reasoning_delta: None,
            tool_call_delta: None,
            usage: Some(hermes_core::Usage::default()),
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

fn config_for_echo() -> HermesConfig {
    HermesConfig {
        provider: ProviderConfig {
            kind: ProviderKind::Echo,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test]
async fn runtime_uses_default_system_prompt_when_config_omits_it_and_skills_dir_absent() {
    // SAFETY: serialized by the test harness; this test does not run in
    // parallel with any other test that mutates HOME. (`cargo test` runs
    // integration tests in separate processes by default.)
    unsafe { std::env::set_var("HOME", "/definitely/does/not/exist/hermes-test") };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let agent = AIAgent::new(provider, config_for_echo());
    let session = SessionContext {
        working_dir: PathBuf::from("/tmp"),
        session_id: "t".into(),
    };
    agent
        .run_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let system = msgs
        .iter()
        .find(|m| m.role == hermes_core::message::Role::System)
        .expect("a System message should have been injected");
    let text = match &system.content {
        hermes_core::message::Content::Text(s) => s.clone(),
        _ => panic!("system message should be text"),
    };
    assert!(
        text.contains("careful assistant"),
        "expected DEFAULT_SYSTEM_PROMPT, got: {text}",
    );
    assert!(
        !text.contains("Available skills"),
        "no skills block expected: {text}",
    );
}

#[tokio::test]
async fn runtime_appends_skills_block_after_user_supplied_system_prompt() {
    let home = tempfile::tempdir().unwrap();
    let skills = skills_dir_for(home.path());
    write_skill(
        &skills,
        "rust-core-style/SKILL.md",
        "---\nname: rust-core-style\ndescription: \"Rust style\"\n---\nbody\n",
    );

    // SAFETY: see prior test.
    unsafe { std::env::set_var("HOME", home.path()) };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let mut config = config_for_echo();
    config.agent.system_prompt = Some("CUSTOM-PROMPT-MARKER".into());
    let agent = AIAgent::new(provider, config);

    let session = SessionContext {
        working_dir: PathBuf::from("/tmp"),
        session_id: "t".into(),
    };
    agent
        .run_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let system = msgs
        .iter()
        .find(|m| m.role == hermes_core::message::Role::System)
        .unwrap();
    let text = match &system.content {
        hermes_core::message::Content::Text(s) => s.clone(),
        _ => panic!("system message should be text"),
    };
    let custom_idx = text.find("CUSTOM-PROMPT-MARKER").expect("custom prompt present");
    let skills_idx = text.find("Available skills").expect("skills block present");
    assert!(
        custom_idx < skills_idx,
        "skills block should follow the user's prompt: {text}",
    );
    assert!(
        text.contains("**rust-core-style**: Rust style"),
        "skill entry missing: {text}",
    );
}

#[tokio::test]
async fn runtime_does_not_fail_construction_when_skills_dir_has_parse_errors() {
    let home = tempfile::tempdir().unwrap();
    let skills = skills_dir_for(home.path());
    write_skill(&skills, "bad-fm/SKILL.md", "no frontmatter at all\n");
    write_skill(&skills, "ok-skill/SKILL.md",
               "---\nname: ok-skill\ndescription: \"fine\"\n---\nbody\n");

    unsafe { std::env::set_var("HOME", home.path()) };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let agent = AIAgent::new(provider, config_for_echo());
    let session = SessionContext {
        working_dir: PathBuf::from("/tmp"),
        session_id: "t".into(),
    };
    agent
        .run_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let system = msgs
        .iter()
        .find(|m| m.role == hermes_core::message::Role::System)
        .unwrap();
    let text = match &system.content {
        hermes_core::message::Content::Text(s) => s.clone(),
        _ => panic!("system message should be text"),
    };
    // ok-skill is loaded, bad-fm is skipped, AIAgent still constructed.
    assert!(text.contains("**ok-skill**"), "good skill should be loaded: {text}");
    assert!(!text.contains("bad-fm"), "bad skill should be skipped: {text}");
}

#[tokio::test]
async fn runtime_uses_default_system_prompt_when_home_is_unset() {
    // SAFETY: serialized by the test harness.
    unsafe { std::env::remove_var("HOME") };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let agent = AIAgent::new(provider, config_for_echo());
    let session = SessionContext {
        working_dir: PathBuf::from("/tmp"),
        session_id: "t".into(),
    };
    agent
        .run_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let system = msgs
        .iter()
        .find(|m| m.role == hermes_core::message::Role::System)
        .expect("a System message should have been injected");
    let text = match &system.content {
        hermes_core::message::Content::Text(s) => s.clone(),
        _ => panic!("system message should be text"),
    };
    assert!(text.contains("careful assistant"));
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

    unsafe { std::env::set_var("HOME", home.path()) };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let agent = AIAgent::new(provider, config_for_echo());
    let session = SessionContext {
        working_dir: PathBuf::from("/tmp"),
        session_id: "t".into(),
    };
    agent
        .run_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let system = msgs
        .iter()
        .find(|m| m.role == hermes_core::message::Role::System)
        .unwrap();
    let text = match &system.content {
        hermes_core::message::Content::Text(s) => s.clone(),
        _ => panic!("system message should be text"),
    };
    assert!(text.contains("Available skills"), "{text}");
    assert!(text.contains("rust-core-style"), "{text}");
    assert!(text.contains("software-engineering.dogfood") || text.contains("dogfood"), "{text}");
    assert!(text.contains("skill_view"), "{text}");
}